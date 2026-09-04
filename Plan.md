# Create hardlinks in another directory for jellyfin

## Plan

**Needs**
* Don't change the underlying file, just create hard links from those underlying source files
* .yaml file for config. Named metadata_corrector.yaml located in same directory as script application 
    * to set source directory to scan and create the links for
    * set output directory for modified named hard links
* Logging changes to output file named changes-mmddyyyy_hhmm.log

* For now just have support for creating hard links for one individual group of media at a time, eg. one show at a time.
    * Add a season and episode number to each episode inside of the season subdirs of the specified dir.
    * Get season numbers from the subdirectory name inside the specified root folder.
        * Don't modify the season folder names
    * Format must be as follows [S01E01] make sure any other season number or episode number is removed from the file name.
* Optional batch mode to process every show folder inside a source root into a matching output root.
    * Enable with `process_all_folders: true` and configure `source_root_directory` and `output_root_directory`.


**Edge Cases**
* Prevent duplicate episodes or seasons from being created.