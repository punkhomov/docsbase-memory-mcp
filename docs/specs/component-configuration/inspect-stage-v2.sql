-- Diagnostic only. Caller supplies observations from the bound parent under
-- admission. Missing requires a checked absence; any failed probe is unavailable.
WITH evidence AS (
    SELECT
        parent_identity = :parent AND profile = :profile AS context_matches,
        (:stage_kind = 'missing' AND :stage_id IS NULL AND :stage_hash IS NULL
            AND :stage_attrs IS NULL) AS stage_absent,
        CASE previous_exists
            WHEN 0 THEN :target_kind = 'missing' AND :target_id IS NULL AND :target_hash IS NULL AND :target_attrs IS NULL
            ELSE :target_kind = 'file' AND previous_object = :target_id
                AND previous_hash = :target_hash AND previous_attributes = :target_attrs
        END AS previous_matches,
        (:target_kind = 'file' AND object_id = :target_id
            AND expected_hash = :target_hash AND expected_attributes = :target_attrs) AS target_matches,
        (:stage_kind = 'file' AND object_id = :stage_id
            AND expected_hash = :stage_hash AND expected_attributes = :stage_attrs) AS stage_matches
    FROM outstanding_stage
    WHERE operation_id = :operation AND ordinal = :ordinal AND direction = :direction
)
SELECT CASE
    WHEN context_matches AND previous_matches AND stage_absent AND target_matches IS NOT TRUE
        THEN 'previous_target_stage_absent'
    WHEN context_matches AND previous_matches AND stage_matches AND target_matches IS NOT TRUE
        THEN 'previous_target_stage_matches'
    WHEN context_matches AND target_matches AND stage_absent AND previous_matches IS NOT TRUE
        THEN 'target_matches_stage_absent'
    ELSE 'inspect_required'
END FROM evidence;
