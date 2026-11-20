--------------------------- MODULE ConversationClose ---------------------------
EXTENDS Naturals, Sequences
CONSTANT MaxWords
ASSUME MaxWords \in Nat \ {0}
VARIABLES words, scan, sawFarewell, sawOther, phase, marked, timer, drained
vars == <<words, scan, sawFarewell, sawOther, phase, marked, timer, drained>>

\* "Farewell" is an explicit phrase, "Polite" is an address or sign-off modifier.
\* Unknown text and question marks both have the "Other" category.
Kinds == {"Farewell","Polite","Other"}
Init == /\ words \in UNION {[1..n -> Kinds] : n \in 1..MaxWords}
        /\ scan = 1 /\ sawFarewell = FALSE /\ sawOther = FALSE /\ phase = "Classifying"
        /\ marked = FALSE /\ timer = FALSE /\ drained = FALSE

Read == /\ phase = "Classifying" /\ scan <= Len(words)
        /\ sawFarewell' = (sawFarewell \/ words[scan] = "Farewell")
        /\ sawOther' = (sawOther \/ words[scan] = "Other")
        /\ scan' = scan+1 /\ UNCHANGED <<words, phase, marked, timer, drained>>

Plan == /\ phase = "Classifying" /\ scan > Len(words)
        /\ phase' = "Answer" /\ marked' = (sawFarewell /\ ~sawOther)
        /\ UNCHANGED <<words, scan, sawFarewell, sawOther, timer, drained>>

Tick == /\ phase = "Answer" /\ marked /\ ~timer /\ timer' = TRUE
        /\ UNCHANGED <<words, scan, sawFarewell, sawOther, phase, marked, drained>>
Drain == /\ phase = "Answer" /\ ~drained /\ drained' = TRUE
         /\ UNCHANGED <<words, scan, sawFarewell, sawOther, phase, marked, timer>>
Resolve == /\ phase = "Answer" /\ drained /\ (~marked \/ timer)
           /\ phase' = IF marked THEN "Quiet" ELSE "Open"
           /\ UNCHANGED <<words, scan, sawFarewell, sawOther, marked, timer, drained>>

Next == Read \/ Plan \/ Tick \/ Drain \/ Resolve \/ (phase \in {"Open","Quiet"} /\ UNCHANGED vars)
Spec == Init /\ [][Next]_vars /\ WF_vars(Read) /\ WF_vars(Plan) /\ WF_vars(Tick)
             /\ WF_vars(Drain) /\ WF_vars(Resolve)

\* Independent oracle on the whole utterance. A story mentioning a farewell contains Other.
FarewellOnly == /\ \E i \in DOMAIN words : words[i] = "Farewell"
                /\ \A i \in DOMAIN words : words[i] \in {"Polite","Farewell"}
OnlyFarewellsClose == (phase = "Quiet" => FarewellOnly /\ timer /\ drained)
GoodbyeIsHonored == (phase = "Open" => ~FarewellOnly)
TurnResolves == phase = "Classifying" ~> phase \in {"Open","Quiet"}
================================================================================
