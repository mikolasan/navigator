use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use clap::{Parser, Subcommand};
use rusqlite::{Connection, Result as SqlResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use html_escape;

#[derive(Parser)]
#[command(name = "html-navigator")]
#[command(about = "A tool to scan HTML files and generate navigation pages")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan directory and generate sitemap
    Scan {
        /// Directory to scan
        #[arg(short, long, default_value = ".")]
        dir: String,
        /// Output file for sitemap
        #[arg(short, long, default_value = "sitemap.html")]
        output: String,
    },
    /// Add comment to a file
    Comment {
        /// File path
        file: String,
        /// Comment text
        comment: String,
    },
    /// Add tags to a file
    Tag {
        /// File path
        file: String,
        /// Tags (comma-separated)
        tags: String,
    },
    /// List all tags
    ListTags,
    /// Generate tag pages
    GenerateTagPages {
        /// Output directory for tag pages
        #[arg(short, long, default_value = "tags")]
        output_dir: String,
    },
    /// Show file metadata
    Show {
        /// File path
        file: String,
    },
    /// Remove file from database
    Remove {
        /// File path
        file: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HtmlFile {
    path: PathBuf,
    title: String,
    modified: SystemTime,
    size: u64,
    hash: String,
}

#[derive(Debug, Clone)]
struct FileMetadata {
    file_path: String,
    hash: String,
    comment: Option<String>,
    tags: Vec<String>,
}

struct Database {
    conn: Connection,
}

impl Database {
    fn new(db_path: &str) -> SqlResult<Self> {
        let conn = Connection::open(db_path)?;
        
        conn.execute(
            "CREATE TABLE IF NOT EXISTS files (
                id INTEGER PRIMARY KEY,
                file_path TEXT UNIQUE NOT NULL,
                hash TEXT NOT NULL,
                comment TEXT,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS tags (
                id INTEGER PRIMARY KEY,
                file_id INTEGER,
                tag TEXT NOT NULL,
                FOREIGN KEY(file_id) REFERENCES files(id),
                UNIQUE(file_id, tag)
            )",
            [],
        )?;

        Ok(Database { conn })
    }

    fn upsert_file(&self, file_path: &str, hash: &str) -> SqlResult<i64> {
        self.conn.execute(
            "INSERT OR REPLACE INTO files (file_path, hash) VALUES (?1, ?2)",
            [file_path, hash],
        )?;
        
        Ok(self.conn.last_insert_rowid())
    }

    fn add_comment(&self, file_path: &str, comment: &str) -> SqlResult<()> {
        self.conn.execute(
            "UPDATE files SET comment = ?, updated_at = CURRENT_TIMESTAMP WHERE file_path = ?",
            [comment, file_path],
        )?;
        Ok(())
    }

    fn add_tags(&self, file_path: &str, tags: &[String]) -> SqlResult<()> {
        let file_id: i64 = self.conn.query_row(
            "SELECT id FROM files WHERE file_path = ?",
            [file_path],
            |row| row.get(0),
        )?;

        // Remove existing tags for this file
        self.conn.execute(
            "DELETE FROM tags WHERE file_id = ?",
            [file_id],
        )?;

        // Add new tags
        for tag in tags {
            self.conn.execute(
                "INSERT OR IGNORE INTO tags (file_id, tag) VALUES (?, ?)",
                [&file_id.to_string(), tag],
            )?;
        }

        Ok(())
    }

    fn get_file_metadata(&self, file_path: &str) -> SqlResult<Option<FileMetadata>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.file_path, f.hash, f.comment FROM files f WHERE f.file_path = ?"
        )?;
        
        let file_meta = stmt.query_row([file_path], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        });

        match file_meta {
            Ok((path, hash, comment)) => {
                let tags = self.get_file_tags(&path)?;
                Ok(Some(FileMetadata {
                    file_path: path,
                    hash,
                    comment,
                    tags,
                }))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn get_file_tags(&self, file_path: &str) -> SqlResult<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.tag FROM tags t 
             JOIN files f ON t.file_id = f.id 
             WHERE f.file_path = ?"
        )?;
        
        let tag_iter = stmt.query_map([file_path], |row| {
            Ok(row.get::<_, String>(0)?)
        })?;

        let mut tags = Vec::new();
        for tag in tag_iter {
            tags.push(tag?);
        }
        Ok(tags)
    }

    fn get_all_tags(&self) -> SqlResult<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT tag FROM tags ORDER BY tag")?;
        let tag_iter = stmt.query_map([], |row| {
            Ok(row.get::<_, String>(0)?)
        })?;

        let mut tags = Vec::new();
        for tag in tag_iter {
            tags.push(tag?);
        }
        Ok(tags)
    }

    fn get_files_by_tag(&self, tag: &str) -> SqlResult<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.file_path FROM files f 
             JOIN tags t ON f.id = t.file_id 
             WHERE t.tag = ? ORDER BY f.file_path"
        )?;
        
        let file_iter = stmt.query_map([tag], |row| {
            Ok(row.get::<_, String>(0)?)
        })?;

        let mut files = Vec::new();
        for file in file_iter {
            files.push(file?);
        }
        Ok(files)
    }

    fn remove_file(&self, file_path: &str) -> SqlResult<()> {
        let file_id_result: Result<i64, _> = self.conn.query_row(
            "SELECT id FROM files WHERE file_path = ?",
            [file_path],
            |row| row.get(0),
        );

        if let Ok(file_id) = file_id_result {
            self.conn.execute("DELETE FROM tags WHERE file_id = ?", [file_id])?;
            self.conn.execute("DELETE FROM files WHERE id = ?", [file_id])?;
        }

        Ok(())
    }
}

fn calculate_file_hash<P: AsRef<Path>>(path: P) -> std::io::Result<String> {
    let content = fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&content);
    Ok(format!("{:x}", hasher.finalize()))
}

fn extract_title_from_html(content: &str) -> String {
    let content_lower = content.to_lowercase();
    
    if let Some(start) = content_lower.find("<title>") {
        if let Some(end) = content_lower[start..].find("</title>") {
            let title_start = start + 7; // length of "<title>"
            let title_end = start + end;
            
            if title_start < content.len() && title_end <= content.len() {
                let title = &content[title_start..title_end];
                let title = html_escape::decode_html_entities(title);
                let title = title.trim().replace('\n', " ").replace('\r', "");
                
                if title.len() > 100 {
                    format!("{}...", &title[..97])
                } else if title.len() == 0 {
                    "No Title".to_string()
                } else {
                    title
                }
            } else {
                "Untitled".to_string()
            }
        } else {
            "Untitled".to_string()
        }
    } else {
        "Untitled".to_string()
    }
}

fn scan_html_files<P: AsRef<Path>>(dir: P) -> std::io::Result<Vec<HtmlFile>> {
    let mut html_files = Vec::new();
    scan_directory_recursive(dir.as_ref(), &mut html_files)?;
    
    // Sort by modification time (newest first)
    html_files.sort_by(|a, b| b.modified.cmp(&a.modified));
    
    Ok(html_files)
}

fn scan_directory_recursive(dir: &Path, html_files: &mut Vec<HtmlFile>) -> std::io::Result<()> {
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            
            if path.is_dir() {
                scan_directory_recursive(&path, html_files)?;
            } else if let Some(extension) = path.extension() {
                if extension == "html" || extension == "htm" {
                    if let Ok(metadata) = fs::metadata(&path) {
                        if let Ok(modified) = metadata.modified() {
                            if let Ok(content) = fs::read_to_string(&path) {
                                let title = extract_title_from_html(&content);
                                let hash = calculate_file_hash(&path).unwrap_or_default();
                                
                                html_files.push(HtmlFile {
                                    path: path.clone(),
                                    title,
                                    modified,
                                    size: metadata.len(),
                                    hash,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn generate_sitemap_html(files: &[HtmlFile], db: &Database, base_dir: &Path) -> String {
    let mut html = String::new();
    
    html.push_str("<!DOCTYPE html>\n");
    html.push_str("<html lang=\"en\">\n");
    html.push_str("<head>\n");
    html.push_str("    <meta charset=\"UTF-8\">\n");
    html.push_str("    <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n");
    html.push_str("    <title>Site Navigation</title>\n");
    html.push_str("    <style>\n");
    html.push_str("        body { font-family: Arial, sans-serif; margin: 2rem; background: #f5f5f5; }\n");
    html.push_str("        .container { max-width: 1200px; margin: 0 auto; background: white; padding: 2rem; border-radius: 8px; box-shadow: 0 2px 10px rgba(0,0,0,0.1); }\n");
    html.push_str("        h1 { color: #333; border-bottom: 3px solid #007acc; padding-bottom: 0.5rem; }\n");
    html.push_str("        .file-list { list-style: none; padding: 0; }\n");
    html.push_str("        .file-item { margin: 1rem 0; padding: 1rem; border: 1px solid #ddd; border-radius: 5px; background: #fafafa; }\n");
    html.push_str("        .file-item:hover { background: #f0f8ff; }\n");
    html.push_str("        .file-title { font-size: 1.2em; font-weight: bold; color: #007acc; }\n");
    html.push_str("        .file-path { color: #666; font-family: monospace; margin: 0.5rem 0; }\n");
    html.push_str("        .file-meta { color: #888; font-size: 0.9em; }\n");
    html.push_str("        .file-tags { margin-top: 0.5rem; }\n");
    html.push_str("        .tag { display: inline-block; background: #007acc; color: white; padding: 0.2rem 0.5rem; margin: 0.1rem; border-radius: 3px; font-size: 0.8em; }\n");
    html.push_str("        .comment { margin-top: 0.5rem; font-style: italic; color: #555; }\n");
    html.push_str("        .stats { margin-top: 2rem; padding: 1rem; background: #e8f4f8; border-radius: 5px; }\n");
    html.push_str("        .stats h2 { margin-top: 0; color: #005577; }\n");
    html.push_str("    </style>\n");
    html.push_str("</head>\n");
    html.push_str("<body>\n");
    html.push_str("    <div class=\"container\">\n");
    html.push_str("        <h1>📄 Site Navigation</h1>\n");
    
    if files.is_empty() {
        html.push_str("        <p>No HTML files found.</p>\n");
    } else {
        html.push_str("        <ul class=\"file-list\">\n");
        
        for file in files {
            // Generate relative path for href
            let relative_path = if let Ok(rel_path) = file.path.strip_prefix(base_dir) {
                rel_path.to_string_lossy().to_string()
            } else {
                file.path.to_string_lossy().to_string()
            };
            
            html.push_str("            <li class=\"file-item\">\n");
            html.push_str(&format!("                <div class=\"file-title\"><a href=\"{}\">{}</a></div>\n", 
                html_escape::encode_text(&relative_path),
                html_escape::encode_text(&file.title)
            ));
            html.push_str(&format!("                <div class=\"file-path\">{}</div>\n", 
                html_escape::encode_text(&relative_path)
            ));
            
            if let Ok(time) = file.modified.duration_since(SystemTime::UNIX_EPOCH) {
                let datetime = chrono::DateTime::from_timestamp(time.as_secs() as i64, 0)
                    .unwrap_or_default();
                html.push_str(&format!("                <div class=\"file-meta\">Modified: {} | Size: {} bytes</div>\n", 
                    datetime.format("%Y-%m-%d %H:%M:%S"),
                    file.size
                ));
            }
            
            // Add metadata if available
            if let Ok(Some(metadata)) = db.get_file_metadata(&file.path.to_string_lossy()) {
                if !metadata.tags.is_empty() {
                    html.push_str("                <div class=\"file-tags\">\n");
                    for tag in &metadata.tags {
                        html.push_str(&format!("                    <span class=\"tag\">{}</span>\n", 
                            html_escape::encode_text(tag)
                        ));
                    }
                    html.push_str("                </div>\n");
                }
                
                if let Some(comment) = &metadata.comment {
                    html.push_str(&format!("                <div class=\"comment\">{}</div>\n", 
                        html_escape::encode_text(comment)
                    ));
                }
            }
            
            html.push_str("            </li>\n");
        }
        
        html.push_str("        </ul>\n");
    }
    
    // Statistics
    html.push_str("        <div class=\"stats\">\n");
    html.push_str("            <h2>📊 Statistics</h2>\n");
    html.push_str(&format!("            <p><strong>Total HTML files:</strong> {}</p>\n", files.len()));
    
    let total_size: u64 = files.iter().map(|f| f.size).sum();
    html.push_str(&format!("            <p><strong>Total size:</strong> {} bytes ({:.2} KB)</p>\n", 
        total_size, total_size as f64 / 1024.0
    ));
    
    if let Some(newest) = files.first() {
        if let Ok(time) = newest.modified.duration_since(SystemTime::UNIX_EPOCH) {
            let datetime = chrono::DateTime::from_timestamp(time.as_secs() as i64, 0)
                .unwrap_or_default();
            let newest_relative = if let Ok(rel_path) = newest.path.strip_prefix(base_dir) {
                rel_path.to_string_lossy().to_string()
            } else {
                newest.path.to_string_lossy().to_string()
            };
            html.push_str(&format!("            <p><strong>Most recently modified:</strong> {} ({})</p>\n", 
                html_escape::encode_text(&newest_relative),
                datetime.format("%Y-%m-%d %H:%M:%S")
            ));
        }
    }
    
    if let Ok(tags) = db.get_all_tags() {
        html.push_str(&format!("            <p><strong>Total tags:</strong> {}</p>\n", tags.len()));
    }
    
    html.push_str(&format!("            <p><em>Generated on: {}</em></p>\n", 
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
    ));
    html.push_str("        </div>\n");
    
    html.push_str("    </div>\n");
    html.push_str("</body>\n");
    html.push_str("</html>\n");
    
    html
}

fn generate_tag_pages(db: &Database, output_dir: &str) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(output_dir)?;
    
    let tags = db.get_all_tags()?;
    
    // Generate tag index page
    let mut index_html = String::new();
    index_html.push_str("<!DOCTYPE html>\n<html><head><title>All Tags</title></head><body>\n");
    index_html.push_str("<h1>📋 All Tags</h1>\n<ul>\n");
    
    for tag in &tags {
        index_html.push_str(&format!("<li><a href=\"{}.html\">{}</a></li>\n", 
            html_escape::encode_text(tag), html_escape::encode_text(tag)
        ));
    }
    
    index_html.push_str("</ul>\n</body></html>");
    fs::write(format!("{}/index.html", output_dir), index_html)?;
    
    // Generate individual tag pages
    for tag in &tags {
        let files = db.get_files_by_tag(tag)?;
        let mut tag_html = String::new();
        
        tag_html.push_str("<!DOCTYPE html>\n<html><head>");
        tag_html.push_str(&format!("<title>Tag: {}</title></head><body>\n", html_escape::encode_text(tag)));
        tag_html.push_str(&format!("<h1>🏷️ Files tagged with '{}'</h1>\n", html_escape::encode_text(tag)));
        tag_html.push_str("<ul>\n");
        
        for file_path in files {
            tag_html.push_str(&format!("<li><a href=\"{}\">{}</a></li>\n", 
                html_escape::encode_text(&file_path), html_escape::encode_text(&file_path)
            ));
        }
        
        tag_html.push_str("</ul>\n<p><a href=\"index.html\">← Back to all tags</a></p>\n");
        tag_html.push_str("</body></html>");
        
        let safe_tag = tag.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_");
        fs::write(format!("{}/{}.html", output_dir, safe_tag), tag_html)?;
    }
    
    println!("Generated {} tag pages in {}/", tags.len(), output_dir);
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let db = Database::new("site_metadata.db")?;
    
    match cli.command {
        Commands::Scan { dir, output } => {
            println!("Scanning directory: {}", dir);
            let base_path = Path::new(&dir);
            let files = scan_html_files(&dir)?;
            
            // Update database with file hashes
            for file in &files {
                let file_path = file.path.to_string_lossy();
                db.upsert_file(&file_path, &file.hash)?;
            }
            
            let html = generate_sitemap_html(&files, &db, base_path);
            fs::write(&output, html)?;
            println!("Generated sitemap: {} ({} files)", output, files.len());
        }
        
        Commands::Comment { file, comment } => {
            let hash = calculate_file_hash(&file)?;
            db.upsert_file(&file, &hash)?;
            db.add_comment(&file, &comment)?;
            println!("Added comment to: {}", file);
        }
        
        Commands::Tag { file, tags } => {
            let hash = calculate_file_hash(&file)?;
            db.upsert_file(&file, &hash)?;
            let tag_list: Vec<String> = tags.split(',').map(|s| s.trim().to_string()).collect();
            db.add_tags(&file, &tag_list)?;
            println!("Added tags to {}: {:?}", file, tag_list);
        }
        
        Commands::ListTags => {
            let tags = db.get_all_tags()?;
            if tags.is_empty() {
                println!("No tags found.");
            } else {
                println!("All tags:");
                for tag in tags {
                    let files = db.get_files_by_tag(&tag)?;
                    println!("  {} ({} files)", tag, files.len());
                }
            }
        }
        
        Commands::GenerateTagPages { output_dir } => {
            generate_tag_pages(&db, &output_dir)?;
        }
        
        Commands::Show { file } => {
            if let Some(metadata) = db.get_file_metadata(&file)? {
                println!("File: {}", metadata.file_path);
                println!("Hash: {}", metadata.hash);
                if let Some(comment) = &metadata.comment {
                    println!("Comment: {}", comment);
                }
                if !metadata.tags.is_empty() {
                    println!("Tags: {}", metadata.tags.join(", "));
                }
            } else {
                println!("No metadata found for: {}", file);
            }
        }
        
        Commands::Remove { file } => {
            db.remove_file(&file)?;
            println!("Removed metadata for: {}", file);
        }
    }
    
    Ok(())
}

