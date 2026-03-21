## Kanban Agentic Harness Design

We are building an extension for the mu agentic harness. Command will be /kanban <folder_name>

Here's what I want to realize:

- this will put mu into a looping mode where it will monitor the folder structure
- folder names will initially be: DRAFT, TODO, FEEDBACK (if user feedback required), PROCESSING, RESULT, REFINE
   - DRAFT: user can edit .md files here, with no action from the model; user can reference items there
   - TODO: model will take in documents placed in TODO and start processing them; document will be renamed DOCUMENT_NAME_UUIDV7 to be able to keep track
   - FEEDBACK: documents that need clarification/feedback from the user are placed here
   - RESULT: will create a folder for the output (can be a simple response.md, task result or a software project, depending on the query), basically using DOCUMENT_NAME_UUIDV7
   - REFINE: user can create a DOCUMENT_NAME_UIDV7_COMMENTS.md here in case a project needs a second pass (kanban is smart enough to know which project to reference)
- folder STATS, will have a STATS.md tracking some statistics

