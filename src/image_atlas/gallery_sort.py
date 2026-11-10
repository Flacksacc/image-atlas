"""Whitelisted gallery ordering and grouping expressions.

Only entries in these mappings may be interpolated into SQL.
Group labels and person-ID semantics match the native viewer.
"""

SORT_FIELDS = {
    'Relevance': ('Newest indexed', 'i.indexed_at'),
    'DateTaken': ('Date taken', 'i.capture_at'),
    'DateModified': ('Modified', 'i.mtime_ns'),
    'DateIndexed': ('Indexed', 'i.indexed_at'),
    'LastViewed': ('Last viewed', 'i.last_viewed_at'),
    'Filename': ('Filename', 'i.filename_sort'),
    'Path': ('Path', 'lower(i.path)'),
    'Folder': ('Folder', 'lower(i.folder_path)'),
    'Bytes': ('File size', 'i.size_bytes'),
    'Width': ('Width', 'i.width'),
    'Height': ('Height', 'i.height'),
    'Pixels': ('Pixels', '(i.width::bigint*i.height::bigint)'),
    'Aspect': ('Aspect ratio', '(i.width::float8/nullif(i.height,0))'),
    'Format': ('Format', 'i.format'),
    'Camera': ('Camera', 'i.camera_model'),
    'Rating': ('Rating', 'i.rating'),
    'Faces': ('Face count', '(SELECT count(*) FROM faces f WHERE f.image_id=i.id)'),
    'Duplicates': ('Exact duplicates', 'CASE WHEN i.sha256 IS NULL THEN 1 ELSE (SELECT count(*) FROM images copies WHERE copies.sha256=i.sha256) END'),
    'FolderCount': ('Folder image count', '(SELECT count(*) FROM images siblings WHERE siblings.folder_path=i.folder_path)'),
    'FolderBytes': ('Folder total bytes', '(SELECT coalesce(sum(size_bytes),0) FROM images siblings WHERE siblings.folder_path=i.folder_path)'),
    'Random': ('Stable shuffle', 'md5(i.id::text)'),
}

GROUP_FIELDS = {
    'None': "''::text",
    'People': "coalesce((SELECT string_agg(label, ', ' ORDER BY label) FROM (SELECT DISTINCT p.name || ' (#' || p.id || ')' AS label FROM faces f JOIN people p ON p.id=f.person_id WHERE f.image_id=i.id AND NOT p.ignored) names), CASE WHEN EXISTS(SELECT 1 FROM faces f WHERE f.image_id=i.id AND f.person_id IS NULL) THEN 'Unnamed people' WHEN EXISTS(SELECT 1 FROM faces f WHERE f.image_id=i.id) THEN 'Ignored people' ELSE 'No faces' END)",
    'FaceGroup': "coalesce((SELECT string_agg('Face #' || group_id, ', ' ORDER BY group_id) FROM (SELECT DISTINCT f.group_id FROM faces f LEFT JOIN people p ON p.id=f.person_id WHERE f.image_id=i.id AND NOT coalesce(p.ignored,false)) groups), 'No reviewable faces')",
    'Day': "coalesce(i.capture_day::text,'Unknown date')",
    'Month': "coalesce(to_char(i.capture_day,'YYYY-MM'),'Unknown date')",
    'Year': "coalesce(to_char(i.capture_day,'YYYY'),'Unknown date')",
    'Folder': "coalesce(i.folder_path,'Unknown folder')",
    'Camera': "coalesce(nullif(i.camera_model,''),'Unknown camera')",
    'Format': "coalesce(i.format,'Unknown format')",
    'Rating': "i.rating::text || ' stars'",
    'Review': 'i.review_state',
    'FaceCount': "(SELECT count(*)::text || ' faces' FROM faces f WHERE f.image_id=i.id)",
    'Orientation': "CASE WHEN i.width IS NULL OR i.height IS NULL OR i.width<=0 OR i.height<=0 THEN 'Unknown dimensions' WHEN i.width=i.height THEN 'Square' WHEN i.width>i.height THEN 'Landscape' ELSE 'Portrait' END",
    'Resolution': "CASE WHEN i.width IS NULL OR i.height IS NULL OR i.width<=0 OR i.height<=0 THEN 'Unknown resolution' WHEN i.width::bigint*i.height<1000000 THEN '1 · Below 1 MP' WHEN i.width::bigint*i.height<4000000 THEN '2 · 1–4 MP' WHEN i.width::bigint*i.height<12000000 THEN '3 · 4–12 MP' ELSE '4 · 12 MP and above' END",
    'FileSize': "CASE WHEN i.size_bytes<1048576 THEN '1 · Below 1 MiB' WHEN i.size_bytes<10485760 THEN '2 · 1–10 MiB' WHEN i.size_bytes<104857600 THEN '3 · 10–100 MiB' ELSE '4 · 100 MiB and above' END",
}
for _kind in ('event', 'trip', 'place'):
    GROUP_FIELDS[_kind.title()] = (
        "coalesce((SELECT string_agg(label, ', ' ORDER BY label) FROM "
        "(SELECT coalesce(c.user_name,c.suggested_name) || ' (#' || c.id || ')' AS label "
        "FROM collection_images m JOIN collections c ON c.id=m.collection_id "
        f"WHERE m.image_id=i.id AND m.source<>'excluded' AND c.status<>'dismissed' AND c.kind='{_kind}') names), "
        f"'No {_kind}')")


def oriented_sql(sql):
    sql = sql.replace('i.width', '__DISPLAY_WIDTH__').replace('i.height', '__DISPLAY_HEIGHT__')
    rotate = "i.metadata_version<5 AND i.exif->>'Orientation' IN ('5','6','7','8')"
    return sql.replace('__DISPLAY_WIDTH__', f'(CASE WHEN {rotate} THEN i.height ELSE i.width END)').replace('__DISPLAY_HEIGHT__', f'(CASE WHEN {rotate} THEN i.width ELSE i.height END)')
