-------------------------- MODULE ListeningGate --------------------------
EXTENDS Naturals
CONSTANT MaxTokens
ASSUME MaxTokens \in Nat \ {0}
VARIABLES phase, issued, owner, capture, roomEvent
vars == <<phase, issued, owner, capture, roomEvent>>

Init == /\ phase = "Idle" /\ issued = 0 /\ owner = 0 /\ capture = 0
        /\ roomEvent = FALSE

\* Face, gaze and ambient sound can update passive behaviour.
Observe == /\ roomEvent' = ~roomEvent
           /\ UNCHANGED <<phase, issued, owner, capture>>

Wake == /\ phase \in {"Idle", "Reminder"} /\ issued < MaxTokens
        /\ issued' = issued + 1 /\ owner' = issued'
        /\ phase' \in {"WakeAck", "Listening"}
        /\ UNCHANGED <<capture, roomEvent>>

Remind == /\ phase = "Idle" /\ phase' = "Reminder"
          /\ UNCHANGED <<issued, owner, capture, roomEvent>>

EndLine == /\ phase \in {"WakeAck", "Reminder"}
           /\ phase' = IF phase = "WakeAck" THEN "Listening" ELSE "Idle"
           /\ UNCHANGED <<issued, owner, capture, roomEvent>>

\* A VAD onset carries the capture token it was observed under.
Onset(t) == /\ t \in 1..issued
            /\ IF phase = "Listening" /\ t = owner
               THEN /\ phase' = "Recording" /\ capture' = t
               ELSE UNCHANGED <<phase, capture>>
            /\ UNCHANGED <<issued, owner, roomEvent>>

Submit == /\ phase = "Recording" /\ phase' = "Submitted" /\ capture' = 0
          /\ UNCHANGED <<issued, owner, roomEvent>>

Stop == /\ phase # "Idle" /\ phase' = "Idle" /\ owner' = 0 /\ capture' = 0
        /\ UNCHANGED <<issued, roomEvent>>

Next == Observe \/ Wake \/ Remind \/ EndLine \/ Submit \/ Stop \/ \E t \in 1..issued : Onset(t)
Spec == Init /\ [][Next]_vars /\ WF_vars(EndLine)

OnlyIntentAuthorizes == (owner # 0) = (phase \in {"WakeAck","Listening","Recording","Submitted"})
OnlyFreshInput == /\ (capture # 0) = (phase = "Recording")
                  /\ (phase = "Recording" => capture = owner /\ owner = issued)
RoomDoesNotWake == [][(roomEvent' # roomEvent) => UNCHANGED <<phase, owner, capture>>]_vars
LineEventuallyEnds == (phase = "WakeAck" ~> phase # "WakeAck") /\
                      (phase = "Reminder" ~> phase # "Reminder")
=============================================================================
