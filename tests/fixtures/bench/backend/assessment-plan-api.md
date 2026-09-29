# Assessment Plan API

The assessment plan service exposes CRUD endpoints for plans and their items.

## Identifiers

Every plan is addressed by its `assessment_plan_id`, a stable opaque string:

```json
{
  "assessment_plan_id": "ap_9f3c21",
  "title": "Quarterly review"
}
```

The `assessment_plan_id` is generated once and never reused, so clients may
cache plan lookups by that key indefinitely.

## Endpoints

- `GET /v1/assessment-plans/{assessment_plan_id}` returns the plan.
- `PATCH /v1/assessment-plans/{assessment_plan_id}` updates the plan title.

## Errors

A missing identifier yields `404 assessment_plan_not_found`.
