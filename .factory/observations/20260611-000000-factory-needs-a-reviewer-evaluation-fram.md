2026-06-11 — Factory needs a reviewer evaluation framework. The
write→review loop's correctness depends on many prompt-shaping and
contract decisions we currently make on intuition: prior-review
framing ("a previous review of this candidate" vs "your previous
review"), Progress quaternary vs binary, lenient vs strict no-progress
quorum, the Pass > Uncertain > Fail ordering, where to place the prior
review in the prompt, reviewer set narrowing across rounds, etc. None
of these are verifiable today. Sketch of an eval framework: (1) per-
reviewer synthetic corpus of (candidate workspace, prior review,
ground-truth verdict, ground-truth progress signal) for regression
testing; (2) A/B reviewer comparisons that hold corpus fixed and vary
one prompt knob at a time; (3) recursive evaluation via post-merge
reviewers catching things attempt-time reviewers missed (built-in
false-negative signal); (4) self-consistency probes — run the same
reviewer on the same candidate K times, measure verdict stability.
The Factory's own Work model can drive eval runs as Work Items whose
Tasks are reviewer invocations against the corpus. This is its own
track, not blocking the slice-1 unification.
