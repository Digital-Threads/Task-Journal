---
description: Map the project into modules and sort past tasks into them (Task Journal chronicle)
argument-hint: "[area to focus on]"
---

Build or extend this project's module map in Task Journal: the parts of the system by
meaning, each with its history of tasks.

Focus (optional): $ARGUMENTS

1. Call `module_list`. If modules exist, extend the map rather than starting over.
2. Study the project: layout, README and docs, the key code paths.
3. Propose modules as a short list — for each: id (lowercase slug like `stars`,
   `auth-refresh`), name, one to three sentences on what it is, `hints.paths` (code path
   prefixes) and `hints.terms` (words people use for it). Ask the user to confirm or
   correct the list, and wait for the answer.
4. After the user's yes, `module_save` each confirmed module.
5. Call `module_backfill_candidates(limit=50)`, page by page (`offset` steps past a page).
   Assign each task to modules using its title, goal, outcome, files and the suggestions;
   list the doubtful ones separately.
6. Show the assignment to the user and ask them to confirm it. After a yes, make one
   `module_link` call per page. Tasks that fit no module go to a catch-all module
   (`other`, "Other") once the user agrees, so no task is left without a module.
7. For every module with tasks, read `module_page` and write its first `state` with
   `module_save(module_id, state=...)`: a few sentences on how it works now.

Finish with `module_list` and tell the user what the map holds and what is left.
