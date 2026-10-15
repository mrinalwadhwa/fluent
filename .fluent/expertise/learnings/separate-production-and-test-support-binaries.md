---
name: separate-production-and-test-support-binaries
description: Build production and test-support Fluent binaries in separate target directories during the declared Tester suite
metadata:
  type: testing
---

The declared Tester suite needs both a production Fluent binary and a binary
built with `test-support`. Build them under `target/production` and
`target/test-support` and point `FLUENT_PRODUCTION_BIN` and
`FLUENT_BIN_OVERRIDE` to those distinct files. Building both variants at
`target/debug/fluent` can replace a binary while a scheduler fixture is
running. Scheduler reuse compares the current executable's digest with its
recorded build identity, so that replacement makes a live coordinator appear
to have a different build.

Keep the separate paths in shell harnesses and tests that inspect the declared
Tester commands. Related: [[shared-service-ownership-spans-drain]].
