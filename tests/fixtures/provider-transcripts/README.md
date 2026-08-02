# Legacy scheduler outage fixtures

These fixtures preserve the 13-event Claude outage transcript shared by the
failed Review Tasks in these two scheduler Attempts:

- `20260730-175933-persistent-scheduler-service-attempt-submit-attach/attempt-1`
- `20260730-175933-persistent-scheduler-service-multi-project-dispatch/attempt-1`

Fixture `a` comes from the first Attempt's architecture review. Fixture `b`
comes from the second Attempt's architecture review. The other eight Review
transcripts have the same normalized structure and safety-relevant values.

Normalization changes only volatile or local values:

- working directory, session ids, event ids, and the assistant message id;
- retry delays and terminal durations;
- local plugin installation paths, while retaining each plugin name and source.

It retains every field and value that can show model output, tool activity,
permission activity, token use, provider work, cost, or terminal semantics.
After the normalization above and removal of the volatile fields, all ten
source transcripts and
`claude-scheduler-outage-structural-manifest.jsonl` have SHA-256 digest
`5450b0f7633c2fb587daaf7047b468e03e4a9eac90b650fb5d7f5e34b4a99994`.
