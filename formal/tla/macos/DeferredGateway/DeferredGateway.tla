---------------------------- MODULE DeferredGateway ----------------------------
\* Mac gateway waits that used to block the connection branch. A plain-audio
\* commit can arrive before the last PCM frame. Its deadline and every new
\* foreground turn or cancel advance one counter. A late frame cannot admit
\* against an old counter. A failure or reminder line prepared for a previous
\* counter, or while a turn is still active, is not published.
EXTENDS Naturals
CONSTANT MaxFrames
ASSUME MaxFrames \in Nat \ {0}

VARIABLES phase, sent, received, fence, commitFence, prep, prepFence, admitted, published, returnedFence, expected
vars == <<phase, sent, received, fence, commitFence, prep, prepFence, admitted, published, returnedFence, expected>>

Init == /\ phase = "open" /\ sent = 0 /\ received = 0 /\ fence = 1 /\ commitFence = 0
        /\ prep = "idle" /\ prepFence = 0 /\ admitted = FALSE /\ published = FALSE
        /\ returnedFence = 0 /\ expected = 0

Speak == /\ phase = "open" /\ sent < MaxFrames /\ sent' = sent + 1
         /\ UNCHANGED <<received, fence, commitFence, prep, prepFence, admitted, published, returnedFence, phase, expected>>
CommitEarly == /\ phase = "open" /\ received < sent /\ phase' = "committed" /\ commitFence' = fence
               /\ admitted' = FALSE /\ expected' = sent
               /\ UNCHANGED <<sent, received, fence, prep, prepFence, published, returnedFence>>
Receive == /\ phase \in {"open", "committed"} /\ received < sent /\ received' = received + 1
           /\ UNCHANGED <<phase, sent, fence, commitFence, prep, prepFence, admitted, published, returnedFence, expected>>
\* The commit is current. Starting the turn advances the counter, so anything
\* prepared for the previous turn cannot publish afterwards.
Admit == /\ phase = "committed" /\ received = expected /\ received > 0 /\ commitFence = fence /\ fence < 3
         /\ phase' = "admitted" /\ admitted' = TRUE /\ fence' = fence + 1
         /\ UNCHANGED <<sent, received, commitFence, prep, prepFence, published, returnedFence, expected>>
FinishTurn == /\ phase = "admitted" /\ phase' = "open" /\ admitted' = FALSE
              /\ UNCHANGED <<sent, received, fence, commitFence, prep, prepFence, published, returnedFence, expected>>
Stop == /\ phase \in {"open", "committed", "admitted"} /\ fence < 3
        /\ phase' = "stopped" /\ fence' = fence + 1 /\ admitted' = FALSE
        /\ UNCHANGED <<sent, received, commitFence, prep, prepFence, published, returnedFence, expected>>
\* The plain-audio deadline. The capture is cleared and the commit is not admitted.
Expire == /\ phase = "committed" /\ fence < 3
          /\ phase' = "expired" /\ fence' = fence + 1 /\ admitted' = FALSE
          /\ UNCHANGED <<sent, received, commitFence, prep, prepFence, published, returnedFence, expected>>
LateFrame == /\ phase \in {"stopped", "expired"} /\ received < sent /\ received' = received + 1
             /\ UNCHANGED <<phase, sent, fence, commitFence, prep, prepFence, admitted, published, returnedFence, expected>>
StartPrep == /\ prep = "idle" /\ prep' = "preparing" /\ prepFence' = fence
             /\ UNCHANGED <<phase, sent, received, fence, commitFence, admitted, published, returnedFence, expected>>
\* A turn-scoped cancel, a stop, or a new turn moves the counter. The
\* preparation that was already in flight keeps the counter it started with.
CancelPrep == /\ fence < 3 /\ (prep = "preparing" \/ phase = "committed")
              /\ fence' = fence + 1
              /\ phase' = IF phase = "committed" THEN "stopped" ELSE phase
              /\ UNCHANGED <<sent, received, commitFence, prep, prepFence, admitted, published, returnedFence, expected>>
\* Published only for the current counter, and only once the foreground turn
\* is gone. A stale failure or reminder stays silent.
PrepReturns == /\ prep = "preparing"
               /\ LET fresh == prepFence = fence /\ phase = "open" IN
                  /\ prep' = IF fresh THEN "published" ELSE "discarded"
                  /\ published' = fresh
                  /\ returnedFence' = fence
               /\ UNCHANGED <<phase, sent, received, fence, commitFence, prepFence, admitted, expected>>

\* A second request cannot replace the declaration or extend the first deadline.
DuplicateCommit == /\ phase = "committed" /\ UNCHANGED vars

Next == Speak \/ CommitEarly \/ Receive \/ Admit \/ FinishTurn \/ Stop \/ Expire \/ LateFrame
        \/ StartPrep \/ CancelPrep \/ PrepReturns \/ DuplicateCommit \/ UNCHANGED vars
Spec == Init /\ [][Next]_vars

StoppedDoesNotAdmit == phase \in {"stopped", "expired"} => ~admitted
AdmittedCompletesTheCommit == admitted => commitFence < fence
ExactDeclaredFrames == admitted => received = expected
StalePrepIsSilent == prep = "discarded" => ~published
PublishedMatchesItsFence == published => prep = "published" /\ returnedFence = prepFence
================================================================================
