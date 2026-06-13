2026-06-08 — Work Attempt follow-up loops reran the full reviewer set
after every small follow-up writer Task. That preserved quality, but it
made review loops slow when only one reviewer finding changed. The first
slice should keep the full required reviewer set as the merge-queue
safety gate while narrowing intermediate Attempt review rounds to the
failed reviewer roles that fed the follow-up write Task, with a
conservative fallback to the full reviewer set when provenance cannot be
derived.
→ Resolved: `66db98c` and `b895a08` added targeted follow-up review
planning in the Work Attempt loop, deriving roles from completed review
Task producer ids in follow-up `input_artifacts`, falling back to the
full reviewer set when mappings are missing, preserving full initial and
merge-time reviews, updating architecture/behavior docs, and adding
unit, binary, and operation behavior coverage.
