---
name: semantic-validation-tests-reach-target-branch
description: Semantic rejection tests must use structurally valid persisted input and assert the target validation error so deserialization cannot satisfy them accidentally
metadata:
  type: testing
---

When a persisted request passes through deserialization before semantic
validation, a regression for a semantic rule must supply a complete,
structurally valid serialized record. If the fixture itself cannot deserialize,
the test can observe an error without ever reaching the rule it claims to cover.

After calling the production validation boundary, assert a stable diagnostic
that identifies the intended check, such as an identity mismatch or an empty
required field. This distinguishes the semantic branch from parse and schema
failures while keeping the test at the public interface.

Related: [[test-names-match-assertions]],
[[negative-test-cross-product-coverage]].
