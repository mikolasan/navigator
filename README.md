# Sitemap generator

## intro

There are so many HTML files on one server, but I don't think people can find them without a direct link. 
So I was thinking about a navigation page. 

Also I wanted to add some meta information like, my comments and tags, to the final list, but keep it separately from the files of the website (SQLite database)

## capabilities

This program scans recursively for all .html files in a directory and gathers their titles, modification time, size and outputs as a nice list formatted as HTML.

Page titles extracted from the <title> tags (truncated if longer than 100 characters).
List is sorted by modification time, displaying the fresh entries first.

Also it generates special pages: one with a list of all tags and list of articles for each tag. 
