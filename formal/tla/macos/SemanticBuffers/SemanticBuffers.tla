-------------------------- MODULE SemanticBuffers --------------------------
EXTENDS Naturals, Sequences
CONSTANT Capacity, TotalBudget
VARIABLES accepted, queue, delivered, overflow, terminal
vars == <<accepted, queue, delivered, overflow, terminal>>
Init == /\ accepted = <<>> /\ queue = <<>> /\ delivered = <<>>
    /\ overflow = FALSE /\ terminal = "open"
Accept ==
    /\ ~overflow /\ terminal = "open"
    /\ Len(queue) < Capacity /\ Len(accepted) < TotalBudget
    /\ accepted' = Append(accepted,Len(accepted)+1)
    /\ queue' = Append(queue,Len(accepted)+1)
    /\ UNCHANGED <<delivered,overflow,terminal>>
Overflow ==
    /\ ~overflow /\ terminal = "open"
    /\ Len(queue) = Capacity \/ Len(accepted) = TotalBudget
    /\ overflow' = TRUE
    /\ UNCHANGED <<accepted,queue,delivered,terminal>>
Drain ==
    /\ Len(queue)>0 /\ terminal = "open"
    /\ delivered' = Append(delivered,Head(queue)) /\ queue' = Tail(queue)
    /\ UNCHANGED <<accepted,overflow,terminal>>
Fail ==
    /\ overflow /\ queue = <<>> /\ terminal = "open"
    /\ terminal' = "error"
    /\ UNCHANGED <<accepted,queue,delivered,overflow>>
Finish ==
    /\ ~overflow /\ queue = <<>> /\ terminal = "open"
    /\ terminal' = "done"
    /\ UNCHANGED <<accepted,queue,delivered,overflow>>
Next == Accept \/ Overflow \/ Drain \/ Fail \/ Finish \/ UNCHANGED vars
Spec == Init /\ [][Next]_vars /\ WF_vars(Drain) /\ WF_vars(Fail)
BoundedQueues == Len(queue)<=Capacity /\ Len(accepted)<=TotalBudget
NoMissingMiddle == delivered \o queue = accepted
TerminalAfterPrefix == terminal # "open" => delivered = accepted
OverflowCannotBeSuccess == overflow => terminal # "done"
OverflowIsExplicit == overflow ~> terminal = "error"
=============================================================================
