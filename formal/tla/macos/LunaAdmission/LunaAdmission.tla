--------------------------- MODULE LunaAdmission ---------------------------
EXTENDS Naturals, FiniteSets
CONSTANT Requests, Capacity, HttpAllowed
VARIABLES state, owner, emitted
vars == <<state, owner, emitted>>
Active == {r \in Requests : state[r] \in {"waiting", "luna", "http"}}
Init == /\ state = [r \in Requests |-> "new"] /\ owner = 0 /\ emitted = {}
Admit(r) ==
    /\ state[r] = "new"
    /\ state' = [state EXCEPT ![r] = IF Cardinality(Active) < Capacity THEN "waiting" ELSE "busy"]
    /\ UNCHANGED <<owner, emitted>>
Acquire(r) ==
    /\ state[r] = "waiting" /\ owner = 0
    /\ state' = [state EXCEPT ![r] = "luna"] /\ owner' = r
    /\ UNCHANGED emitted
\* Local timeout does not assume the previous Luna generation ever finishes.
Expire(r) ==
    /\ state[r] = "waiting"
    /\ state' = [state EXCEPT ![r] = IF HttpAllowed THEN "http" ELSE "busy"]
    /\ UNCHANGED <<owner, emitted>>
Speak(r) ==
    /\ state[r] \in {"luna", "http"}
    /\ emitted' = emitted \cup {r}
    /\ UNCHANGED <<state, owner>>
Finish(r) ==
    /\ r \in Active
    /\ state' = [state EXCEPT ![r] = "done"]
    /\ owner' = IF owner = r THEN 0 ELSE owner
    /\ UNCHANGED emitted
Stop(r) ==
    /\ r \in Active
    /\ state' = [state EXCEPT ![r] = "cancelled"]
    /\ owner' = IF owner = r THEN 0 ELSE owner
    /\ UNCHANGED emitted
Next == (\E r \in Requests : Admit(r) \/ Acquire(r) \/ Expire(r) \/ Speak(r) \/ Finish(r) \/ Stop(r))
    \/ UNCHANGED vars
Spec == Init /\ [][Next]_vars /\ \A r \in Requests : WF_vars(Expire(r))
BoundedSemanticAdmission == Cardinality(Active) <= Capacity
SingleLunaThread == Cardinality({r \in Requests : state[r] = "luna"}) <= 1
LunaOwnerIsExact == \A r \in Requests : state[r] = "luna" <=> owner = r
NoRoutePolicyExpansion == ~HttpAllowed => \A r \in Requests : state[r] # "http"
NoSpeechFromRejectedInput == \A r \in emitted : state[r] \notin {"new", "waiting", "busy"}
WaitingIsBounded == \A r \in Requests : state[r] = "waiting" ~> state[r] # "waiting"
=============================================================================
