# Testing in the factory

The factory has several types of tests:

**Behavioral tests** (`tests/behaviors/`) — verify the factory
delivers its specified behaviors. Written from EARS statements
without seeing code. Test the system from the outside.

**Skill tests** (`tests/test-skill`) — simulate skill conversations
between two agents. Test skill structure and flow.

Each type catches a different class of problems. Behavioral tests
catch user-visible regressions. Skill tests catch conversation
design issues.
