------------------------ MODULE UtteranceCommit ------------------------
EXTENDS Naturals
CONSTANT MaxFrames
ASSUME MaxFrames \in Nat \ {0}
VARIABLES phase, sent, received, pauseWire, pauseAt, attempted, candidate, transcript
vars == <<phase, sent, received, pauseWire, pauseAt, attempted, candidate, transcript>>

Init == /\ phase = "Speaking" /\ sent = 0 /\ received = 0
        /\ pauseWire = 0 /\ pauseAt = 0 /\ attempted = FALSE /\ candidate = 0 /\ transcript = 0

Speak == /\ phase \in {"Speaking","Draining"} /\ sent < MaxFrames
         /\ sent' = sent+1 /\ phase' = "Speaking"
         /\ UNCHANGED <<received, pauseWire, pauseAt, attempted, candidate, transcript>>

Pause == /\ phase = "Speaking" /\ sent > 0 /\ phase' = "Draining"
         /\ pauseWire' = IF pauseWire = 0 THEN sent ELSE pauseWire
         /\ UNCHANGED <<sent, received, pauseAt, attempted, candidate, transcript>>

\* One WSS pause per capture. Its last PCM frame might still be in transit.
ReceivePause == /\ phase \in {"Speaking","Draining","CommitSent"}
                /\ pauseWire > 0 /\ pauseWire >= received /\ pauseAt = 0 /\ ~attempted
                /\ pauseAt' = pauseWire
                /\ UNCHANGED <<phase, sent, received, pauseWire, attempted, candidate, transcript>>

ReceiveFrame == /\ phase \in {"Speaking","Draining","CommitSent"} /\ received < sent
                /\ received' = received+1
                /\ candidate' = IF received' > pauseAt THEN 0 ELSE candidate
                /\ pauseAt' = IF received' > pauseAt THEN 0 ELSE pauseAt
                /\ UNCHANGED <<phase, sent, pauseWire, attempted, transcript>>

TryASR == /\ phase \in {"Speaking","Draining","CommitSent"} /\ ~attempted
          /\ pauseAt = received /\ received > 0 /\ attempted' = TRUE
          /\ candidate' \in {0, received}  \* busy or successful
          /\ UNCHANGED <<phase, sent, received, pauseWire, pauseAt, transcript>>

SendCommit == /\ phase = "Draining" /\ phase' = "CommitSent"
              /\ UNCHANGED <<sent, received, pauseWire, pauseAt, attempted, candidate, transcript>>

Admit == /\ phase = "CommitSent" /\ received = sent
         /\ transcript' = IF candidate # 0 THEN candidate ELSE received
         /\ phase' = "Admitted"
         /\ UNCHANGED <<sent, received, pauseWire, pauseAt, attempted, candidate>>

Stop == /\ phase \in {"Speaking","Draining","CommitSent"} /\ phase' = "Stopped" /\ candidate' = 0
        /\ UNCHANGED <<sent, received, pauseWire, pauseAt, attempted, transcript>>
Terminal == phase \in {"Stopped","Admitted"} /\ UNCHANGED vars
Next == Speak \/ Pause \/ ReceivePause \/ ReceiveFrame \/ TryASR \/ SendCommit \/ Admit \/ Stop \/ Terminal
Spec == Init /\ [][Next]_vars /\ WF_vars(ReceiveFrame) /\ WF_vars(Admit)

FreshPrefix == candidate = 0 \/ candidate = received
CompleteUtterance == (transcript > 0) = (phase = "Admitted") /\
                     (phase = "Admitted" => transcript = sent /\ received = sent)
CommitBeforeAnswer == [][(phase' = "Admitted" /\ phase # "Admitted") => phase = "CommitSent"]_vars
DrainsAfterCommit == phase = "CommitSent" ~> phase \in {"Admitted","Stopped"}
=============================================================================
