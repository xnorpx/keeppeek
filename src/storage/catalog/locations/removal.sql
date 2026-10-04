SELECT 1 FROM (
    SELECT source_operation AS operation FROM storage_volume_moves
    WHERE phase NOT IN ('complete','cancelled') OR receipt_acknowledged=0
    UNION ALL
    SELECT destination_operation FROM storage_volume_moves
    WHERE phase NOT IN ('complete','cancelled') OR receipt_acknowledged=0
    UNION ALL
    SELECT operation FROM storage_image_retirements WHERE acknowledged=0
    UNION ALL
    SELECT operation FROM storage_recording_retirements WHERE acknowledged=0
    UNION ALL
    SELECT operation FROM storage_recording_recovery WHERE acknowledged=0
    UNION ALL
    SELECT a.operation FROM storage_export_cleanup c
    JOIN storage_volume_allocations a ON a.kind='export' AND a.object_id=c.object_id
    WHERE c.acknowledged=0
) pending
JOIN storage_volume_allocations allocation ON allocation.operation=pending.operation
WHERE allocation.volume_id=?1 LIMIT 1
