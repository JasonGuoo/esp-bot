---------------------------- MODULE DeferredGateway ----------------------------
\* Mac gateway waits that used to block the connection branch: a commit can
\* arrive before the last PCM frame, and a failure or reminder line can be
\* prepared after its foreground owner has already been cleared. Stop and a
\* turn-scoped cancel raise the fence. A late frame or a late preparation
\* cannot admit or publish against the old fence.
EXTENDS Naturals
CONSTANT MaxFrames
ASSUME MaxFrames \in Nat \ {0}

VARIABLES phase, sent, received, fence, prep, prepFence, admitted, published, returnedFence
vars == <<phase, sent, received, fence, prep, prepFence, admitted, published, returnedFence>>

Init == /\ phase = "open" /\ sent = 0 /\ received = 0 /\ fence = 1
        /\ prep = "idle" /\ prepFence = 0 /\ admitted = FALSE /\ published = FALSE
        /\ returnedFence = 0

Speak == /\ phase = "open" /\ sent < MaxFrames /\ sent' = sent + 1
         /\ UNCHANGED <<received, fence, prep, prepFence, admitted, published, returnedFence, phase>>
CommitEarly == /\ phase = "open" /\ received < sent /\ phase' = "committed"
               /\ UNCHANGED <<sent, received, fence, prep, prepFence, admitted, published, returnedFence>>
Receive == /\ phase \in {"open", "committed"} /\ received < sent /\ received' = received + 1
           /\ UNCHANGED <<phase, sent, fence, prep, prepFence, admitted, published, returnedFence>>
Admit == /\ phase = "committed" /\ received = sent /\ received > 0
         /\ phase' = "admitted" /\ admitted' = TRUE
         /\ UNCHANGED <<sent, received, fence, prep, prepFence, published, returnedFence>>
Stop == /\ phase \in {"open", "committed"} /\ fence < 3
        /\ phase' = "stopped" /\ fence' = fence + 1
        /\ UNCHANGED <<sent, received, prep, prepFence, admitted, published, returnedFence>>
Expire == /\ phase = "committed" /\ fence < 3
          /\ phase' = "expired" /\ fence' = fence + 1
          /\ UNCHANGED <<sent, received, prep, prepFence, admitted, published, returnedFence>>
LateFrame == /\ phase \in {"stopped", "expired"} /\ received < sent /\ received' = received + 1
             /\ UNCHANGED <<phase, sent, fence, prep, prepFence, admitted, published, returnedFence>>
StartPrep == /\ prep = "idle" /\ prep' = "preparing" /\ prepFence' = fence
             /\ UNCHANGED <<phase, sent, received, fence, admitted, published, returnedFence>>
\* The foreground owner may already be gone. The cancel still moves the fence,
\* and a commit that is still waiting for its tail is not admitted afterwards.
CancelPrep == /\ fence < 3 /\ (prep = "preparing" \/ phase = "committed")
              /\ fence' = fence + 1
              /\ phase' = IF phase = "committed" THEN "stopped" ELSE phase
              /\ UNCHANGED <<sent, received, prep, prepFence, admitted, published, returnedFence>>
PrepReturns == /\ prep = "preparing"
               /\ prep' = IF prepFence = fence THEN "published" ELSE "discarded"
               /\ published' = (prepFence = fence) /\ returnedFence' = fence
               /\ UNCHANGED <<phase, sent, received, fence, prepFence, admitted>>

Next == Speak \/ CommitEarly \/ Receive \/ Admit \/ Stop \/ Expire \/ LateFrame
        \/ StartPrep \/ CancelPrep \/ PrepReturns \/ UNCHANGED vars
Spec == Init /\ [][Next]_vars

StoppedDoesNotAdmit == phase \in {"stopped", "expired"} => ~admitted
AdmittedCompletesTheCommit == admitted => phase = "admitted" /\ received = sent
StalePrepIsSilent == prep = "discarded" => ~published
PublishedMatchesItsFence == published => prep = "published" /\ returnedFence = prepFence
================================================================================
