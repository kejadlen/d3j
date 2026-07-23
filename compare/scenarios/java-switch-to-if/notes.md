# java-switch-to-if

Based on the paper's Figure 13, the one genuine semantic merge its
authors found in a developer study. Left adds a `case 3` to the switch;
right refactors the whole switch into a chain of `if` statements.

The edits target the same structure in incompatible ways: right deleted
the switch that left extended. A conflict is the expected structural
outcome — reconciling them is the semantic refactoring a human performs,
beyond what either tool resolves automatically.
