# java-insert-insert

Based on the paper's Figure 12. Both branches insert a new statement at
the same slot — the end of `greet` — but with different content. The
insertions collide, so an insert-insert conflict is the correct outcome.

Figure 12's point is subtler: it shows a tool duplicating code when one
branch's insertion coincidentally matches the other's edit, yielding a
non-universal conflict-free merge instead of the conflict that should
have been reported. This scenario keeps the collision explicit.
