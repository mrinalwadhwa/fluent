---
name: sandbox-denials-track-template-grants
description: Handoff-only sandbox confinement depends on stripping exact shared-temp grant strings from the rendered profile
metadata:
  type: gotcha
---

`os::render_profile_for_access_for_coder_with_denied_writes` adds explicit deny roots, then removes the broad `/private/var/folders` and `/private/tmp` write grants by replacing their exact rendered Seatbelt text. If `sandbox/common.sb` or profile generation changes the spelling or layout of either grant, the replacement silently becomes a no-op and weakens handoff-only Git confinement.

When changing common sandbox grants or the renderer, update this stripping logic and keep an integration test that proves writes to the denied shared-temp paths actually fail. If refactoring the profile builder, express the exclusion during profile construction so confinement does not depend on post-render string surgery.
