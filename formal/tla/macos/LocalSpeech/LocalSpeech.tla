------------------------------ MODULE LocalSpeech ------------------------------
EXTENDS Naturals, FiniteSets
CONSTANT Lanes
VARIABLES generation, state, owner, offered, cancelled, fallback
vars == <<generation, state, owner, offered, cancelled, fallback>>
Init == /\ generation = 0
        /\ state = [l \in Lanes |-> "Idle"] /\ owner = [l \in Lanes |-> 0]
        /\ offered = {} /\ cancelled = {} /\ fallback = {}
Start(l) == /\ state[l] = "Idle"
            /\ state' = [state EXCEPT ![l] = "Generating"]
            /\ owner' = [owner EXCEPT ![l] = generation]
            /\ UNCHANGED <<generation, offered, cancelled, fallback>>
Finish(l) == /\ state[l] = "Generating"
             /\ state' = [state EXCEPT ![l] = "Ready"]
             /\ UNCHANGED <<generation, owner, offered, cancelled, fallback>>
Fail(l) == /\ state[l] = "Generating"
           /\ state' = [state EXCEPT ![l] = "Fallback"]
           /\ fallback' = fallback \cup {<<l, owner[l]>>}
           /\ UNCHANGED <<generation, owner, offered, cancelled>>
FinishFallback(l) == /\ state[l] = "Fallback"
                     /\ state' = [state EXCEPT ![l] = "Ready"]
                     /\ UNCHANGED <<generation, owner, offered, cancelled, fallback>>
Offer(l) == /\ state[l] = "Ready" /\ owner[l] = generation
            /\ state' = [state EXCEPT ![l] = "Done"]
            /\ offered' = offered \cup {<<l, owner[l]>>}
            /\ UNCHANGED <<generation, owner, cancelled, fallback>>
Stop == /\ generation = 0
        /\ generation' = 1
        /\ cancelled' = cancelled \cup {<<l,owner[l]>> : l \in {k \in Lanes : state[k] \in {"Generating","Fallback","Ready"}}}
        /\ state' = [l \in Lanes |-> "Idle"]
        /\ UNCHANGED <<owner, offered, fallback>>
\* Late old child output is discarded; it cannot complete the new exchange.
LateOldResult == /\ generation = 1 /\ UNCHANGED vars
Next == (\E l \in Lanes : Start(l) \/ Finish(l) \/ Fail(l) \/ FinishFallback(l) \/ Offer(l))
        \/ Stop \/ LateOldResult
Spec == Init /\ [][Next]_vars
NoCancelledSpeech == offered \intersect cancelled = {}
NoPartialSpeech == [][(offered' # offered) => \E l \in Lanes : state[l] = "Ready" /\ owner[l] = generation]_vars
PriorityIndependent == (state["Normal"] \in {"Generating","Fallback"} /\ state["First"] = "Idle") => ENABLED Start("First")
===============================================================================
