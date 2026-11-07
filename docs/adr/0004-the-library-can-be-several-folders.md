# The library can be several folders

A common layout keeps the library as sibling folders beside the downloads folder: `movies`, `tv` and `anime` under one parent that also holds `downloads`. DejaView took one library folder, and the parent could not be it, since a library folder that holds downloads is refused (`ensure_roots_disjoint`). So a download that was a copy of a file in a second folder was never found, and switching the library folder between runs pruned the other folder's cache entries, which meant reading the whole library again over the network. Settled on 2026-11-07 (issue #11).

## What was decided

`media` in the config holds one folder or a list, and every scan and link walks downloads and every library folder at once.

- **The config file.** One folder is written as a string, as before, so a config written by an earlier version reads and writes the same. Several are a list.
- **The checks.** Each library folder passes the checks one did: set, absolute, existing, readable, and not inside downloads nor holding it. Library folders must also be apart from each other, none inside another, and have different names (their last path component).
- **The report.** `media` stays a string, the first library folder, so scripts that read it keep working. A new field, `media_folders`, lists every library folder.
- **The cache.** With one library folder a library entry's key stays `media/<path below it>`, exactly as before, so a single-folder setup sees no change, a Docker mount of the library under another folder name included. With several, the key is `library/<folder name>/<path below it>`, which is why two folders may not share a name. Adding a folder renames the `media/` keys to the folder that holds each path, one `stat` per entry, once; going back to one renames that folder's `library/` keys back. Neither reads a file again.
- **The dashboard.** The path editor gives each library folder a line, with an empty line at the end to add one; clearing a line removes that folder. With several folders the pairs list names each library file's folder in a column of its own.

## Considered options

- **Allow the parent as the library folder, with downloads carved out of it.** Rejected: it weakens the refusal that keeps the library read-only, and the walk of the library would have to skip downloads by path, which a bind mount can defeat.
- **Key every library entry by its folder's name, one folder included.** Rejected: a Docker container that mounts the library under a different folder name than the host would then key the same files differently from the host, and each run would prune the other's entries.
- **Make `media` a list in the report.** Rejected: retyping a field breaks every script that reads it. A new field breaks none.
