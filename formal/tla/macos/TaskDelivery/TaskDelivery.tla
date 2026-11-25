----------------------------- MODULE TaskDelivery -----------------------------
\* A finished task and the foreground turn that asked for it are different.
\* The result is stored with its inbox row. Speaking it needs the current
\* lease and a playback receipt. A lost notice can be scanned again; the
\* effect is not run a second time. Two tasks, one device, a bounded inbox.
EXTENDS Naturals, FiniteSets

Tasks == 1..2
\* status: 0 empty, 1 accepted, 2 terminal, 3 cancelled
\* inbox:  0 absent, 1 pending, 2 offered, 3 delivered, 4 deferred, 5 partial

VARIABLES status, inbox, heard, effect, device, lease, fg, closed, gotReceipt, outcome, day
vars == <<status, inbox, heard, effect, device, lease, fg, closed, gotReceipt, outcome, day>>

Init ==
    /\ status = [t \in Tasks |-> 0]
    /\ inbox = [t \in Tasks |-> 0]
    /\ heard = [t \in Tasks |-> FALSE]
    /\ effect = [t \in Tasks |-> FALSE]
    /\ device = "idle" /\ lease = 0 /\ fg = 0
    /\ closed = FALSE /\ gotReceipt = FALSE /\ outcome = "open" /\ day = 0

Accept(t) ==
    /\ status[t] = 0
    /\ status' = [status EXCEPT ![t] = 1]
    /\ UNCHANGED <<inbox, heard, effect, device, lease, fg, closed, gotReceipt, outcome, day>>

\* The terminal state and the inbox row are one step. The notice may be lost
\* (it is not a variable); the row is the record that a later scan reads.
Finish(t) ==
    /\ status[t] = 1
    /\ status' = [status EXCEPT ![t] = 2]
    /\ inbox' = [inbox EXCEPT ![t] = 1]
    /\ effect' = [effect EXCEPT ![t] = TRUE]
    /\ UNCHANGED <<heard, device, lease, fg, closed, gotReceipt, outcome, day>>

\* Cancel files a pending row. If this task held the lease, that lease ends.
Cancel(t) ==
    /\ status[t] = 1
    /\ status' = [status EXCEPT ![t] = 3]
    /\ inbox' = [inbox EXCEPT ![t] = 1]
    /\ lease' = IF lease = t THEN 0 ELSE lease
    /\ UNCHANGED <<heard, effect, device, fg, closed, gotReceipt, outcome, day>>

NewTurn ==
    /\ fg < 2 /\ device \in {"idle", "committed"}
    /\ fg' = fg + 1 /\ closed' = FALSE /\ gotReceipt' = FALSE /\ outcome' = "open"
    /\ device' = "committed"
    /\ UNCHANGED <<status, inbox, heard, effect, lease, day>>

CloseForeground ==
    /\ fg > 0 /\ ~closed /\ closed' = TRUE
    /\ UNCHANGED <<status, inbox, heard, effect, device, lease, fg, gotReceipt, outcome, day>>

\* The explanation's receipt ends the foreground. It does not finish a task.
HearForeground ==
    /\ closed /\ ~gotReceipt /\ \E t \in Tasks : status[t] = 1
    /\ gotReceipt' = TRUE /\ outcome' = "delegated"
    /\ UNCHANGED <<status, inbox, heard, effect, device, lease, fg, closed, day>>

Claim(t) ==
    /\ inbox[t] \in {1, 4} /\ lease = 0 /\ device = "committed"
    /\ inbox' = [inbox EXCEPT ![t] = 2] /\ lease' = t
    /\ UNCHANGED <<status, heard, effect, device, fg, closed, gotReceipt, outcome, day>>

\* Delivered only from the offered row, on this lease, with a receipt.
Receive(t) ==
    /\ inbox[t] = 2 /\ lease = t
    /\ inbox' = [inbox EXCEPT ![t] = 3] /\ heard' = [heard EXCEPT ![t] = TRUE]
    /\ lease' = 0 /\ device' = "idle"
    /\ UNCHANGED <<status, effect, fg, closed, gotReceipt, outcome, day>>

\* Stop and disconnect both put an offered row back to pending. A stopped or
\* idle device never keeps an offered row.
Stop ==
    /\ device # "stopped"
    /\ device' = "stopped" /\ lease' = 0
    /\ inbox' = [t \in Tasks |-> IF inbox[t] = 2 THEN 1 ELSE inbox[t]]
    /\ UNCHANGED <<status, heard, effect, fg, closed, gotReceipt, outcome, day>>

Disconnect ==
    /\ device' = "idle" /\ lease' = 0
    /\ inbox' = [t \in Tasks |-> IF inbox[t] = 2 THEN 1 ELSE inbox[t]]
    /\ UNCHANGED <<status, heard, effect, fg, closed, gotReceipt, outcome, day>>

\* A summary receipt is not the full result. Yes re-offers it; no or a timeout settles it.
Summarize(t) ==
    /\ inbox[t] = 2 /\ lease = t
    /\ inbox' = [inbox EXCEPT ![t] = 5] /\ lease' = 0 /\ device' = "idle"
    /\ UNCHANGED <<status, heard, effect, fg, closed, gotReceipt, outcome, day>>

Reoffer(t) ==
    /\ inbox[t] = 5 /\ lease = 0 /\ device = "committed"
    /\ inbox' = [inbox EXCEPT ![t] = 2] /\ lease' = t
    /\ UNCHANGED <<status, heard, effect, device, fg, closed, gotReceipt, outcome, day>>

SettlePartial(t) ==
    /\ inbox[t] = 5
    /\ inbox' = [inbox EXCEPT ![t] = 3] /\ heard' = [heard EXCEPT ![t] = TRUE]
    /\ UNCHANGED <<status, effect, device, lease, fg, closed, gotReceipt, outcome, day>>

Midnight ==
    /\ day = 0 /\ day' = 1
    /\ UNCHANGED <<status, inbox, heard, effect, device, lease, fg, closed, gotReceipt, outcome>>

Next ==
    \E t \in Tasks : Accept(t) \/ Finish(t) \/ Cancel(t) \/ Claim(t) \/ Receive(t)
    \/ NewTurn \/ CloseForeground \/ HearForeground \/ Stop \/ Disconnect \/ Midnight
    \/ Summarize(t) \/ Reoffer(t) \/ SettlePartial(t)
    \/ UNCHANGED vars

Spec == Init /\ [][Next]_vars /\ WF_vars(Stop) /\ WF_vars(Disconnect)

TaskLifetimeIndependent ==
    \A t \in Tasks :
        /\ status[t] = 2 => effect[t]
        /\ status[t] = 3 => inbox[t] # 0

ResultAndInboxAtomic ==
    \A t \in Tasks : status[t] = 2 => inbox[t] # 0

NoDeliveryWithoutCurrentLease ==
    \A t \in Tasks : inbox[t] = 2 => lease = t /\ device = "committed"

DeliveredRequiresReceipt ==
    \A t \in Tasks : inbox[t] = 3 => heard[t]

ForegroundCompletionIsHonest ==
    outcome = "delegated" => closed /\ gotReceipt

NoEffectReplayOnRecovery ==
    \A t \in Tasks : effect[t] => status[t] = 2

BoundedDeliveryWork ==
    Cardinality({t \in Tasks : inbox[t] \in {1, 2, 4, 5}}) <= 2

\* An offered row does not stay offered: stop, disconnect, or a receipt moves it.
OfferLeaves == \A t \in Tasks : inbox[t] = 2 ~> inbox[t] # 2
=============================================================================
