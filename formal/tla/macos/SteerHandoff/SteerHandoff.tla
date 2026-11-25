------------------------------ MODULE SteerHandoff ------------------------------
\* A follow-up can be steered into a running turn. The provider may emit the
\* steer's user message before the caller has consumed the RPC acknowledgement.
\* The barrier is installed first. Early notes stay buffered until the ack
\* commits the handoff or the request is withdrawn. A note clears the pending
\* flag only for the steer epoch that captured it.
EXTENDS Naturals

VARIABLES phase, note, owner, pending, delivered, extra, epoch, noteEpoch
vars == <<phase, note, owner, pending, delivered, extra, epoch, noteEpoch>>

Init == /\ phase = "idle" /\ note = "none" /\ owner = "old" /\ pending = 0
        /\ delivered = "none" /\ extra = FALSE /\ epoch = 1 /\ noteEpoch = 0

Arm == /\ phase = "idle" /\ phase' = "armed" /\ note' = "none" /\ noteEpoch' = 0
       /\ UNCHANGED <<owner, pending, delivered, extra, epoch>>
Early == /\ phase = "armed" /\ note = "none" /\ note' = "early" /\ noteEpoch' = epoch
         /\ UNCHANGED <<phase, owner, pending, delivered, extra, epoch>>
Commit == /\ phase = "armed" /\ phase' = "committed" /\ owner' = "new" /\ pending' = epoch
          /\ UNCHANGED <<note, delivered, extra, epoch, noteEpoch>>
Apply == /\ phase = "committed" /\ note = "early" /\ pending /= 0 /\ noteEpoch = pending
         /\ pending' = 0 /\ note' = "applied" /\ delivered' = "new"
         /\ UNCHANGED <<phase, owner, extra, epoch, noteEpoch>>
\* A note captured by an older steer is still queued after a newer one committed.
Stale == /\ phase = "committed" /\ note = "early" /\ noteEpoch /= 0 /\ noteEpoch /= pending
         /\ note' = "none"
         /\ UNCHANGED <<phase, owner, pending, delivered, extra, epoch, noteEpoch>>
FinishOk == /\ phase = "committed" /\ note = "applied" /\ pending = 0 /\ phase' = "done"
            /\ UNCHANGED <<note, owner, pending, delivered, extra, epoch, noteEpoch>>
Unanswered == /\ phase = "committed" /\ pending /= 0 /\ note /= "early"
              /\ phase' = "done" /\ extra' = TRUE /\ delivered' = "fresh"
              /\ UNCHANGED <<note, owner, pending, epoch, noteEpoch>>
Withdraw == /\ phase = "armed" /\ phase' = "withdrawn" /\ note' = "none" /\ noteEpoch' = 0
            /\ UNCHANGED <<owner, pending, delivered, extra, epoch>>
FinishWithdrawn == /\ phase = "withdrawn" /\ phase' = "done" /\ extra' = TRUE
                   /\ delivered' = "fresh"
                   /\ UNCHANGED <<note, owner, pending, epoch, noteEpoch>>
\* One newer steer, so the state space stays finite. It does not retag the old note.
Second == /\ phase = "committed" /\ pending /= 0 /\ epoch < 2
          /\ epoch' = epoch + 1 /\ pending' = epoch'
          /\ UNCHANGED <<phase, note, owner, delivered, extra, noteEpoch>>

Next == Arm \/ Early \/ Commit \/ Apply \/ Stale \/ FinishOk \/ Unanswered
        \/ Withdraw \/ FinishWithdrawn \/ Second
        \/ (phase = "done" /\ UNCHANGED vars)
Spec == Init /\ [][Next]_vars

\* The new owner hears the steered answer only after a committed handoff, once.
HandoffOnce == delivered = "new" =>
    /\ phase \in {"committed", "done"} /\ owner = "new" /\ note = "applied" /\ ~extra
\* A rejected, timed-out, or unanswered steer never publishes into the new sink.
NoLeak == extra => delivered /= "new"
\* Delivery records the epoch that was pending when the note was applied.
DeliveredCurrent == delivered = "new" => noteEpoch /= 0
================================================================================
