------------------------------ MODULE Supplement ------------------------------
EXTENDS Naturals
CONSTANT MaxSegments
ASSUME MaxSegments \in Nat \ {0}
VARIABLES phase, generated, finished, consumed, heard, atCut, intent,
          reservation, relation, questionIncluded, quoted
vars == <<phase, generated, finished, consumed, heard, atCut, intent,
          reservation, relation, questionIncluded, quoted>>

Init == /\ phase = "Original" /\ generated = 0 /\ finished = 0 /\ consumed = 0
        /\ heard = 0 /\ atCut = 0 /\ intent = "None" /\ reservation = FALSE
        /\ relation = "None" /\ questionIncluded = FALSE /\ quoted = 0

Produce == /\ phase = "Original" /\ generated < MaxSegments /\ generated' = generated+1
           /\ UNCHANGED <<phase, finished, consumed, heard, atCut, intent,
                          reservation, relation, questionIncluded, quoted>>
FinishAudio == /\ phase = "Original" /\ finished < generated /\ finished' = finished+1
               /\ UNCHANGED <<phase, generated, consumed, heard, atCut, intent,
                              reservation, relation, questionIncluded, quoted>>

\* A genuine completion may still be in transit when the old output is revoked.
TakeReceipt == /\ consumed < finished /\ consumed' = consumed+1
               /\ heard' = IF phase = "Original" THEN consumed' ELSE heard
               /\ UNCHANGED <<phase, generated, finished, atCut, intent,
                              reservation, relation, questionIncluded, quoted>>

Interrupt(kind) == /\ phase = "Original" /\ phase' = "Between" /\ intent' = kind
                   /\ atCut' = heard /\ reservation' = (kind = "Supplement")
                   /\ UNCHANGED <<generated, finished, consumed, heard, relation, questionIncluded, quoted>>

\* A stop/replace can arrive after the supplement gesture and before the new input.
Withdraw == /\ phase = "Between" /\ reservation /\ reservation' = FALSE /\ intent' = "Stop"
            /\ UNCHANGED <<phase, generated, finished, consumed, heard, atCut, relation, questionIncluded, quoted>>

\* In the gap before audio.begin, the Mac still has the original token.
\* The device may close with that token or with the unfocused direct successor.
CloseTags == {"Current", "Direct", "SkippedTurn", "OtherSession", "OtherEpoch", "OtherFocus"}
CloseDuringHandover(tag) ==
    /\ phase = "Between" /\ reservation /\ relation' = tag
    /\ IF tag \in {"Current", "Direct"}
          THEN /\ phase' = "Closed" /\ reservation' = FALSE /\ intent' = "Stop"
          ELSE UNCHANGED <<phase, reservation, intent>>
    /\ UNCHANGED <<generated, finished, consumed, heard, atCut, questionIncluded, quoted>>

Submit(nextToken) ==
    /\ phase = "Between" /\ phase' = "Next" /\ relation' = nextToken
    /\ questionIncluded' = (reservation /\ nextToken = "Direct")
    /\ quoted' = IF questionIncluded' THEN heard ELSE 0
    /\ reservation' = FALSE
    /\ UNCHANGED <<generated, finished, consumed, heard, atCut, intent>>

LaterTurn == /\ phase = "Next" /\ phase' = "Later" /\ questionIncluded' = FALSE /\ quoted' = 0
             /\ UNCHANGED <<generated, finished, consumed, heard, atCut, intent, reservation, relation>>

Next == Produce \/ FinishAudio \/ TakeReceipt \/ Withdraw \/ LaterTurn
        \/ (\E kind \in {"Supplement","Replace","Stop"} : Interrupt(kind))
        \/ (\E t \in {"Direct","SkippedTurn","OtherSession","OtherEpoch"} : Submit(t))
        \/ (\E tag \in CloseTags : CloseDuringHandover(tag))
        \/ (phase \in {"Later","Closed"} /\ UNCHANGED vars)
Spec == Init /\ [][Next]_vars

PreviousQuestion == questionIncluded = (phase = "Next" /\ intent = "Supplement" /\ relation = "Direct")
OnlyConfirmedSpeech == quoted = (IF questionIncluded THEN atCut ELSE 0)
LateReceiptsStayRetired == (phase # "Original" => heard = atCut)
CloseAccepted == (phase = "Between" => relation \notin {"Current", "Direct"})
CloseIsScoped == (phase = "Closed" => relation \in {"Current", "Direct"} /\ ~reservation /\ ~questionIncluded)
ReservationSpent == (phase \in {"Next","Later"} => ~reservation)
===============================================================================
