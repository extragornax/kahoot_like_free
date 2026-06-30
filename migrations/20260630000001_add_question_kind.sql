-- Question kind: 'choice' (multiple choice, the default), 'slide' (section, no
-- answers), or 'open' (free-text answer followed by a vote). Needed because both
-- slide and open questions have zero answer rows, so answer count alone can no
-- longer tell them apart.
ALTER TABLE questions ADD COLUMN kind TEXT NOT NULL DEFAULT 'choice';

-- Existing answer-less questions were section slides.
UPDATE questions SET kind = 'slide'
WHERE id NOT IN (SELECT DISTINCT question_id FROM answers);
