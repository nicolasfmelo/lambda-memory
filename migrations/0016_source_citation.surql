DEFINE FIELD lesson_count ON memory_session TYPE int DEFAULT 0;
UPDATE memory_session SET lesson_count = 0 WHERE lesson_count = NONE;
DEFINE TABLE memory_source_citation SCHEMAFULL;
DEFINE FIELD owner_id ON memory_source_citation TYPE string;
DEFINE FIELD project_id ON memory_source_citation TYPE string;
DEFINE FIELD source_id ON memory_source_citation TYPE string;
DEFINE FIELD cites ON memory_source_citation TYPE string;
DEFINE INDEX source_citation_pair ON memory_source_citation FIELDS owner_id, project_id, source_id, cites UNIQUE;
DEFINE INDEX source_citation_evidence ON memory_source_citation FIELDS owner_id, project_id, cites;
