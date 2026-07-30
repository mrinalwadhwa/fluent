---
name: json-manifest-paths-via-jq-arg
description: External values written into JSON manifests must use jq --arg, not string interpolation
metadata:
  type: gotcha
---

Interpolating shell variables directly into a JSON document (`jq -n "{ \"key\": \"$val\"
}"`) corrupts the document when the value contains JSON-significant characters such as
`"`, `\`, or newlines. A smoke root like `/tmp/he said "hi"` produces invalid JSON,
making `jq` fail on every subsequent read.

Write manifest fields that carry external values with `jq --arg` (or `--argjson` for
numeric/boolean fields) to let `jq` handle encoding:

```sh
jq -n --arg root "$root" --arg boundary "$boundary" \
   '{ smoke_root: $root, install_boundary: $boundary }'
```

This applies everywhere an external value (path, URL, user-supplied string) is serialized
into a JSON file, not only to manifest creation. See also
[[atomic-manifest-within-smoke-root]] for the write pattern that pairs with this.
