# Architectural principles

Principles for structural decisions at any scale. Consult when making
design choices or reviewing code structure.

## Simplicity

Prefer the simplest solution that works. Not the most clever, not the
most flexible, not the most "clean" — the simplest.

**Essential vs accidental complexity.** Every system has complexity
inherent in the problem (essential) and complexity introduced by the
tools and approaches chosen (accidental). The goal is to minimize
accidental complexity. When something feels complex, ask: is this
the problem being hard, or is this our solution being complicated?

**Implementation simplicity wins.** A design that's easy to build and
maintain beats one that's theoretically elegant but complex to
implement. A good-enough solution that ships and can be improved
incrementally outperforms a perfect design that's hard to build,
hard to port, and hard to change. (Richard Gabriel, "Worse is Better")

**YAGNI — don't build for hypothetical futures.** Features built for
anticipated needs that haven't materialized yet have four costs: the
effort to build them, the delay to features actually needed, the
complexity they add to everything else, and the maintenance burden of
designs that assumed wrong. Only about a third of planned features
actually improve the metrics they target. Build what's needed now.
Keep the code easy to change so you can add things later.
(Martin Fowler / Kent Beck)

YAGNI is not a license to write bad code. It targets unnecessary
features and premature flexibility, not code quality. Investments in
testability, clarity, and maintainability make YAGNI work — they keep
the code malleable enough to add things when they're actually needed.

**When choosing between two approaches that both work, pick the one
that's easier to understand on first reading.** If you need a comment
to explain why an approach was chosen over the obvious one, the
obvious one might have been the right choice.

## Separation of concerns

Each component should focus on one responsibility and hide its
implementation behind a clear interface. When a component does too
many things, changes to one concern force you to understand and risk
breaking all the others.

This applies at every scale: a function should do one thing, a module
should own one area, a service should have one reason to change.

**Information hiding.** A module should conceal the design decisions
most likely to change. The interface exposes only what other modules
need. When the hidden decision changes, only the module's internals
change — everything else stays the same. The criteria for where to
draw module boundaries: not by processing steps, but by what's likely
to change independently. (David Parnas, 1972)

**Cohesion over convenience.** Group things that change together for
the same reasons. Don't group things just because they're used at the
same time, or because they're the same "type" of code (all utilities
together, all constants together). The question is: when this changes,
what else has to change with it?

## Boundaries

Make boundaries explicit. Unclear boundaries lead to accidental
coupling that gets worse over time.

A boundary is where one component ends and another begins. At the
boundary, the two components agree on a contract: an interface, an
API, a data format. Inside the boundary, each component is free to
change independently.

**Depend on interfaces, not internals.** When one component uses
another, it should depend on the public interface — not reach into
internal structures, private functions, or implementation details.
If the dependency can't be expressed through the public interface,
either the interface needs to change or the dependency shouldn't
exist.

**Import paths reveal coupling.** Code that imports
`foo.internal.bar.utils.helper` depends on the internal layout of
`foo`, not just its public interface. When `foo` reorganizes its
internals, everything that reached in breaks. Import from the
module's public surface. If the IDE or agent suggests a deep path,
consider whether that dependency belongs at all.

**This applies within a codebase, not just across packages.** Even
inside the same project, sibling modules should import from each
other's public surface — not reach into each other's internal files.
In Rust, this means importing from a module's `mod.rs` re-exports
(`use crate::auth::Token`) not from internal files
(`use crate::auth::token::Token`). In Python, importing from a
package's `__init__.py` exports, not from internal modules. Each
module defines what it exports through its root; sibling modules
depend on that surface. The module can then reorganize its internals
without breaking imports across the codebase.

**Protect your domain from external models.** When integrating with
external systems, don't let their data model leak into your internal
model. Build a translation layer at the boundary. If the external
system changes its API, only the translation layer updates — the
rest of your system is unaffected. (Anti-corruption layer, from DDD)

**Bounded contexts.** Different parts of a system may use the same
word to mean different things. "Account" in billing is different from
"account" in authentication. Rather than forcing one definition
everywhere, draw explicit boundaries around where each meaning
applies. Communication between contexts uses explicit mapping.
Boundaries typically follow where language changes.
(Eric Evans, Domain-Driven Design)

## Coupling

Coupling is the degree to which changing one component forces changes
in another. Some coupling is necessary. The problem is unnecessary
coupling — components connected for convenience rather than necessity.

**The shared-utils trap.** Two modules have similar code. Someone
extracts it to a shared place (utils, common, shared, helpers). Both
modules now depend on the shared code. Over time the shared code
evolves to serve both modules' needs, accumulating conditionals and
special cases. The modules are now coupled through the shared
dependency — changing one requires considering the other. They looked
independent but aren't.

The fix is not "never share." Share when the shared thing is genuinely
the same concept with a stable interface. Don't share just because
code looks similar. Similarity is not identity.

**Duplication is cheaper than wrong coupling.** Removing duplication
later (when the pattern is genuinely clear) is straightforward.
Untangling premature coupling that locked unrelated concerns together
is hard and risky. When in doubt, duplicate.
(Sandi Metz, "The Wrong Abstraction")

**The wrong abstraction.** A common cycle: developer A spots
duplication and extracts an abstraction. Time passes. Developer B
needs the abstraction to do something slightly different, so they add
a parameter. Developer C adds a conditional. The abstraction
accumulates complexity until it's incomprehensible but everyone is
afraid to touch it because it's shared everywhere.

The fix: inline the abstraction back into every caller. Delete what
each caller doesn't need. Now you can see the actual patterns, and
abstract correctly if a real shared concept exists. "The fastest way
forward is back." (Sandi Metz)

**Don't let "clean code" override engineering judgment.** Removing
duplication can make code worse if the resulting abstraction doesn't
represent a real shared concept. When requirements change, a simple
duplicated implementation "stays easy as cake" while a forced
abstraction becomes "several times more convoluted." Let clean code
guide you, then let it go if it's not serving the actual engineering
needs. (Dan Abramov, "Goodbye Clean Code")

**Avoid hasty abstractions.** Wait until use cases are clear before
extracting common code. "The commonalities will scream at you for
abstraction" when the pattern is genuinely clear. Early abstraction
leads to code that's "basically your whole application in if
statements and loops." Prefer duplication over the wrong abstraction.
Optimize for change first — requirements inevitably shift.
(Kent C. Dodds, AHA Programming)

**The Law of Demeter.** Only talk to your immediate collaborators.
A method should only call methods on: itself, its parameters, objects
it creates, and its direct component objects. Don't chain through
intermediaries (`a.getB().getC().doThing()`) — this couples you to
the entire chain's structure.

## Abstractions

Every abstraction has a cost: someone has to understand it, maintain
it, and work around it when it doesn't fit. Abstractions should earn
their keep.

**All non-trivial abstractions leak.** Every abstraction eventually
requires understanding the underlying system. TCP leaks network
failures. SQL leaks query optimization needs. A wrapper class leaks
the library it wraps. Design with the assumption that your
abstractions will leak, and make the underlying layer accessible when
it does. (Joel Spolsky, "Law of Leaky Abstractions")

**Don't abstract to prevent future duplication.** Abstract to manage
existing complexity. If there's no complexity to manage yet, there's
nothing to abstract. An abstraction that wraps a single
implementation adds a layer of indirection with no benefit.

**When an abstraction requires its consumers to understand the
implementation, the abstraction is leaking.** Either fix the
abstraction's interface or remove it — a bad abstraction is worse
than no abstraction.

## Viewpoints

Architecture looks different through different lenses. Each viewpoint
reveals concerns the others miss. Apply the viewpoints that are
relevant to the decision at hand — not all viewpoints apply to every
change.

Based on Rozanski & Woods:

**Context** — how the system relates to its environment. Users,
external systems, dependencies. Ask: what does this system interact
with? What are the boundaries between this system and the world?

**Functional** — what the system does. Component responsibilities,
how they collaborate, what each one owns. Ask: does each component
have a clear, single responsibility? Can you describe what a component
does in one sentence?

**Information** — how data flows. Who owns what data, how it moves,
where it's transformed. Ask: is data ownership clear? Are
transformations explicit? Does data flow in one direction or bounce
back and forth?

**Concurrency** — runtime behavior. Processes, threads, communication
between concurrent components. Ask: what runs in parallel? Where can
race conditions occur? How do concurrent components communicate?

**Development** — how the code is organized. Module structure,
dependencies, build process, developer experience. Ask: is the code
organized so someone new can find things? Are dependencies explicit?
Can modules be tested independently?

**Deployment** — where and how the system runs. Infrastructure,
networking, runtime configuration. Ask: can the system be deployed
independently? Is configuration handled cleanly? Is deployment
reproducible?

**Operational** — how the system is maintained. Monitoring, debugging,
error handling, logging. Ask: can you tell what the system is doing
from its logs? When something fails, can you diagnose it without
reading the code? Are errors handled explicitly?

## Diagramming with C4

When documenting architecture, use C4 zoom levels to match the
audience and the decision being made:

**Level 1 — Context.** The system as a box, surrounded by users and
external systems. What does the system interact with? Use for
explaining scope and integration points.

**Level 2 — Container.** Zoom in to show runtime components: services,
databases, queues, applications. Use for deployment, integration, and
infrastructure decisions.

**Level 3 — Component.** Zoom into a container to show internal
modules and their relationships. Use for structural decisions within a
service or application.

**Level 4 — Code.** Class or function level. Rarely useful to draw
manually — let the code speak for itself.

Pick the level that matches the decision. Don't draw Level 3 diagrams
for Level 1 decisions. (Simon Brown, C4 Model)

## Domain modeling

When a system models a business domain, the code vocabulary should
match the domain vocabulary.

**Ubiquitous language.** Use the same terms in code, documentation,
and conversation. If the business calls it a "subscription," don't
call it a "recurring_payment_plan" in the code. Inconsistent
vocabulary causes bugs and misunderstandings that no amount of testing
catches. (Eric Evans, DDD)

**Aggregates.** Group domain objects that must change together into
aggregates with a single root entity. Access the group through the
root only. This enforces consistency boundaries — the aggregate
guarantees its invariants.

## Anti-patterns

Patterns that signal structural problems:

**God object.** A component that accumulates responsibilities until it
does too much. Often happens incrementally — each addition is small,
but the aggregate is a component that everything depends on and nobody
can change safely. Detect by counting: how many reasons could this
component change? If the answer is many, it's a god object.

**Circular dependencies.** Component A depends on B, B depends on A.
Neither can be understood, tested, or deployed independently. Usually
signals that the boundary between them is wrong — they should be one
component, or the shared concern should be extracted into a third.

**Big ball of mud.** No discernible structure. Information shared
broadly, concerns mixed, changes ripple unpredictably. The result of
expedient decisions accumulated without architectural review. "A
haphazardly structured, sprawling, sloppy, duct-tape-and-baling-wire,
spaghetti-code jungle." (Foote & Yoder)

**Lava flow.** Dead code that nobody removes because they're afraid
to. Over time, the codebase accumulates layers of unused code that
obscure the living code and make changes risky.

**Leaky abstraction.** An abstraction that requires consumers to
understand the implementation. If changing the implementation breaks
consumers, the abstraction boundary is wrong.

**Premature abstraction.** An abstraction created before the pattern
is clear. Couples consumers to a shared dependency that evolves in
the wrong direction. See the coupling section above.
