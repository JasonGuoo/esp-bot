---------------------------- MODULE GatewayDispatch ----------------------------
\* Independent bounded WSS writer. A TCP write already started can finish after
\* Stop, but queued old frames are discarded and its receipt grants no new turn.
\* Database/preparation isolation is outside this model.
EXTENDS Naturals, Sequences
CONSTANT Capacity
VARIABLES generation, queue, writing, completed, accepted, stopRequested, stopped,
          enqueued, rejected
vars == <<generation, queue, writing, completed, accepted, stopRequested, stopped,
          enqueued, rejected>>

Init == /\ generation = 1 /\ queue = <<>> /\ writing = 0
        /\ completed = 0 /\ accepted = 0 /\ stopRequested = FALSE
        /\ stopped = FALSE /\ enqueued = 0 /\ rejected = FALSE

Enqueue == /\ enqueued < 4 /\ Len(queue) < Capacity
           /\ queue' = Append(queue,generation) /\ enqueued' = enqueued + 1
           /\ UNCHANGED <<generation,writing,completed,accepted,stopRequested,stopped,rejected>>
Full == /\ Len(queue) = Capacity /\ rejected' = TRUE
        /\ UNCHANGED <<generation,queue,writing,completed,accepted,stopRequested,stopped,enqueued>>
RequestStop == /\ ~stopRequested /\ ~stopped /\ stopRequested' = TRUE
               /\ UNCHANGED <<generation,queue,writing,completed,accepted,stopped,enqueued,rejected>>
Stop == /\ stopRequested /\ ~stopped
        /\ stopped' = TRUE /\ generation' = 2 /\ accepted' = 0
        /\ UNCHANGED <<queue,writing,completed,stopRequested,enqueued,rejected>>
StartWrite == /\ writing = 0 /\ Len(queue) > 0
              /\ writing' = IF Head(queue) = generation THEN Head(queue) ELSE 0
              /\ queue' = Tail(queue)
              /\ UNCHANGED <<generation,completed,accepted,stopRequested,stopped,enqueued,rejected>>
\* This action need not be fair: the peer may never read its socket.
FinishWrite == /\ writing # 0 /\ completed' = writing /\ writing' = 0
               /\ UNCHANGED <<generation,queue,accepted,stopRequested,stopped,enqueued,rejected>>
Receipt == /\ completed # 0
           /\ accepted' = IF completed = generation THEN completed ELSE accepted
           /\ UNCHANGED <<generation,queue,writing,completed,stopRequested,stopped,enqueued,rejected>>

Next == Enqueue \/ Full \/ RequestStop \/ Stop \/ StartWrite \/ FinishWrite \/ Receipt
Spec == Init /\ [][Next]_vars /\ WF_vars(Stop)
BoundedEgress == Len(queue) <= Capacity /\ writing \in 0..2
NoStaleReceiptAuthority == accepted = 0 \/ accepted = generation
StopAdvancesFence == stopped => generation = 2
ControlIndependentOfWrite == stopRequested ~> stopped
================================================================================
