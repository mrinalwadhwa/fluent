# Fluent-specific guidance for skills

## Drive interactive skills as one-area-at-a-time conversations

When a Fluent skill involves a user (capturing briefs, defining behaviors, designing approaches, planning work), drive the conversation in small pieces. Each area or decision gets its own turn. Don't produce a document and ask for approval.

This applies through the entire conversation — a common failure mode is starting with small pieces and dumping everything remaining at the end. One question at a time. Let each answer land before moving on.

This pattern fits Fluent's brief → behaviors → approach → plan lifecycle. It doesn't fit autonomous skills like reviewers or code processors.

## Keep planning-wide delegation explicit and provisional

The one-area-at-a-time pattern remains the default. Its sole planning exception
starts when the user explicitly asks Fluent to use its judgment through the rest
of planning. Under that delegation, Fluent may make grounded recommendation-level
choices and advance through provisional Brief, Behaviors, Approach, and Plan
artifacts without intermediate confirmation gates.

Do not infer this exception from `Keep going`, silence, saved drafts, or execution
autonomy. Continue to interrupt for behavior or scope changes, consequential or
difficult-to-reverse tradeoffs, recorded-decision conflicts, missing information
or access, and exceptional policy. Present the complete planning set once at the
end; until the user confirms it, no artifact is approved and no Work Item may be
created. Planning delegation never authorizes Attempt execution or landing.
