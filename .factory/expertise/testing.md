# Testing in the factory

The factory has several types of tests:

**Behavioral tests** (`tests/behaviors/`) — verify the factory
delivers its specified behaviors. Written from EARS statements
without seeing code. Test the system from the outside.

**Operational tests** (`tests/test-run`) — verify the factory
command's mechanics using real functions via the source guard
(`FACTORY_LIB=1`).

**Skill tests** (`tests/test-skill`) — simulate skill conversations
between two agents. Test skill structure and flow.

Each type catches a different class of problems. Behavioral tests
catch user-visible regressions. Operational tests catch machinery
breakage. Skill tests catch conversation design issues.
