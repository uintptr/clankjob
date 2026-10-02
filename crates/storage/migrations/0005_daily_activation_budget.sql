-- The activation budget counts per 24 hours instead of over a case's life
-- (max_activations becomes max_activations_per_day). Cases that kept the former default
-- lifetime cap of 20 get the new default of 100 a day; any other value carries over.
UPDATE cases
SET budgets = json_set(
    json_remove(budgets, '$.max_activations'),
    '$.max_activations_per_day',
    CASE json_extract(budgets, '$.max_activations') WHEN 20 THEN 100 ELSE json_extract(budgets, '$.max_activations') END
)
WHERE json_extract(budgets, '$.max_activations') IS NOT NULL;
