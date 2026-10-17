UPDATE memory_revision_state SET state = 'processing' WHERE state = 'pending_review' RETURN NONE;
