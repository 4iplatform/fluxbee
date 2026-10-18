# General definition — Monitoring of ICH states in IO nodes

> General note that comes from the work on `IO.linkedhelper`.
>
> Goal: record that monitoring the states of its own ICHs is not a Linked Helper peculiarity, but a responsibility that generalizes to IO nodes.

---

# 1. General principle

Each IO node should be able to:

- know which ICHs are its own;
- observe relevant changes in the state of those ICHs;
- and take operational measures based on those changes.

This includes, at least, cases such as:

- enabling automation;
- disabling automation;
- other relevant operational changes of the channel that affect the node's behavior.

---

# 2. Scope of this responsibility

This responsibility implies:

- internal monitoring of the ICH state;
- updating the state observed by the node;
- preparing pending changes for its consumers/adapters;
- and operational reaction by the node when appropriate.

## Examples of operational reaction
- stop consuming or reporting messages for automation;
- enable a flow again;
- report incremental changes to an external adapter;
- collapse pending states per ICH to avoid delivering redundant history.

---

# 3. What this responsibility does NOT imply

It does not necessarily imply that the IO node is responsible for:

- administratively exposing the full list of enabled/disabled ICHs;
- exposing pending ILKs;
- or being the source of truth for those states.

That can stay on the identity/core side and with other administrative mechanisms of the system.

---

# 4. Key distinction

## IO node
- observes;
- reacts;
- keeps a minimal operational state;
- reports incremental changes to whoever needs them.

## Identity/Core
- holds the source of truth for the ICH state;
- governs the administrative exposure of that state;
- decides the global mechanisms to query or change it.

---

# 5. Implication for new IO nodes

When a new IO node is designed, it is worth evaluating explicitly:

1. which ICHs it considers its own;
2. how it will observe the state changes of those ICHs;
3. what minimal operational state it will have to persist;
4. how it will report relevant changes to its consumers/adapters;
5. and which part of the administration of those states stays outside the node.
