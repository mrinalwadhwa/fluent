---
name: native-pi-input-failures-return-handled
description: Reject unacknowledged Pi input with a handled result because its installed runner swallows callback exceptions
metadata:
  type: gotcha
---

The installed Pi ExtensionRunner catches errors thrown by an input callback
and can continue processing that input. The owned extension therefore returns
`{ action: "handled" }` when Fluent does not acknowledge admission, records the
rejection when possible, and shuts down the session. The owner also checks
receiver liveness and heartbeats; a lost receiver or incomplete evidence pauses
the Task instead of starting another generation after uncertain side effects.

When changing this boundary, exercise the installed extension runner and the
owned Task path. Calling the callback directly does not prove that rejected
input stayed out of the model.

Related: [[native-input-settlement-binds-identity]],
[[external-adapter-contract-tests-drive-real-protocol]].
