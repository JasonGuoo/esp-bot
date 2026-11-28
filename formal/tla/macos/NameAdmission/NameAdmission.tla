--------------------------- MODULE NameAdmission ---------------------------
EXTENDS Naturals
CONSTANT Names, ValidNames
VARIABLES saved, receipt, spoken, revision
vars == <<saved,receipt,spoken,revision>>
\* A legacy name may be invalid; migration preserves it for owner correction.
Init == /\ saved \in Names /\ receipt = FALSE /\ spoken = {} /\ revision = 0
Write(name) ==
    /\ name \in ValidNames /\ revision < 2
    /\ saved' = name /\ receipt' = TRUE /\ revision' = revision+1
    /\ UNCHANGED spoken
Reject(name) == /\ name \notin ValidNames /\ UNCHANGED vars
Speak == /\ saved \in ValidNames /\ spoken' = spoken \cup {saved}
    /\ UNCHANGED <<saved,receipt,revision>>
Next == (\E name \in Names : Write(name) \/ Reject(name)) \/ Speak \/ UNCHANGED vars
Spec == Init /\ [][Next]_vars
OnlyValidatedNamesWritten == receipt => saved \in ValidNames
OnlyValidatedNamesSpoken == spoken \subseteq ValidNames
PolicyReceiptIsAtomic == revision > 0 <=> receipt
=============================================================================
