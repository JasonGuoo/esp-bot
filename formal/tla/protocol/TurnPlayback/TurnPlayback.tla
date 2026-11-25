--------------------------- MODULE TurnPlayback ---------------------------
EXTENDS Naturals, Sequences

CONSTANTS MaxTurns, MaxSegments
ASSUME MaxTurns \in Nat \ {0} /\ MaxSegments \in Nat \ {0}

VARIABLES turn, segment, last, narrated, media, runtime, sawEnd, audioQ, endQ, delegated, taskRunning
vars == <<turn, segment, last, narrated, media, runtime, sawEnd, audioQ, endQ, delegated, taskRunning>>

\* A turn names a fresh runtime token. A segment number doubles as the local
\* playback id; deliberately reuse the same numbers in successive turns.
EmptyMedia == [displayed |-> FALSE, started |-> FALSE,
               drained |-> FALSE, reported |-> FALSE]
Device(mode) == [mode |-> mode, playback |-> 0, endPending |-> FALSE]
Current == <<turn, segment>>
InAnswer == runtime.mode = "Answer"

Init ==
    /\ turn = 1 /\ segment = 1 /\ last \in 1..MaxSegments /\ narrated \in BOOLEAN
    /\ media = EmptyMedia /\ runtime = Device("Answer") /\ sawEnd = FALSE
    /\ audioQ = <<>> /\ endQ = <<>>
    /\ delegated \in BOOLEAN /\ taskRunning = delegated

\* Model one optional duplicate per notification. FIFO order within each
\* queue is preserved; their relative service order is independent.
Enqueue(q, msg) == {Append(q, msg), Append(Append(q, msg), msg)}

Display ==
    /\ InAnswer /\ ~media.displayed
    /\ media' = [media EXCEPT !.displayed = TRUE]
    /\ UNCHANGED <<turn, segment, last, narrated, runtime, sawEnd, audioQ, endQ, delegated, taskRunning>>

StartAudio ==
    /\ InAnswer /\ narrated /\ media.displayed /\ ~media.started
    /\ runtime.playback = 0
    /\ runtime' = [runtime EXCEPT !.playback = segment]
    /\ media' = [media EXCEPT !.started = TRUE]
    /\ UNCHANGED <<turn, segment, last, narrated, sawEnd, audioQ, endQ, delegated, taskRunning>>

HardwareDone ==
    /\ InAnswer /\ media.started /\ ~media.drained
    /\ media' = [media EXCEPT !.drained = TRUE]
    /\ audioQ' \in Enqueue(audioQ, Current)
    /\ UNCHANGED <<turn, segment, last, narrated, runtime, sawEnd, endQ, delegated, taskRunning>>

\* The collector observes real device receipts. Intermediate reports only
\* release the next segment. The final seal also posts PRESENTATION_DONE.
ReportSegment ==
    /\ InAnswer /\ ~media.reported /\ media.displayed
    /\ (~narrated \/ media.drained)
    /\ media' = [media EXCEPT !.reported = TRUE]
    /\ IF segment = last THEN endQ' \in Enqueue(endQ, turn) ELSE endQ' = endQ
    /\ UNCHANGED <<turn, segment, last, narrated, runtime, sawEnd, audioQ, delegated, taskRunning>>

NextSegment ==
    /\ InAnswer /\ media.reported /\ segment < last
    /\ segment' = segment + 1 /\ narrated' \in BOOLEAN /\ media' = EmptyMedia
    /\ UNCHANGED <<turn, last, runtime, sawEnd, audioQ, endQ, delegated, taskRunning>>

TakeAudio ==
    /\ audioQ # <<>> /\ audioQ' = Tail(audioQ)
    /\ runtime' = IF InAnswer /\ Head(audioQ) = <<turn, runtime.playback>>
                  THEN IF runtime.endPending THEN Device("Followup")
                       ELSE [runtime EXCEPT !.playback = 0]
                  ELSE runtime
    /\ UNCHANGED <<turn, segment, last, narrated, media, sawEnd, endQ, delegated, taskRunning>>

TakeEnd ==
    /\ endQ # <<>> /\ endQ' = Tail(endQ)
    /\ LET current == InAnswer /\ Head(endQ) = turn
       IN /\ sawEnd' = (sawEnd \/ current)
          /\ runtime' = IF ~current THEN runtime
                        ELSE IF runtime.playback # 0
                             THEN [runtime EXCEPT !.endPending = TRUE]
                             ELSE Device("Followup")
    /\ UNCHANGED <<turn, segment, last, narrated, media, audioQ, delegated, taskRunning>>

Stop ==
    /\ runtime.mode # "Stopped" /\ runtime' = Device("Stopped")
    /\ UNCHANGED <<turn, segment, last, narrated, media, sawEnd, audioQ, endQ, delegated, taskRunning>>

\* A supplement interrupts an answer. From Stopped a fresh wake is needed.
\* From Followup this represents an authorized next question and its answer.
NewTurn ==
    /\ turn < MaxTurns /\ turn' = turn + 1
    /\ segment' = 1 /\ last' \in 1..MaxSegments /\ narrated' \in BOOLEAN
    /\ media' = EmptyMedia /\ runtime' = Device("Answer") /\ sawEnd' = FALSE
    /\ UNCHANGED <<audioQ, endQ, delegated, taskRunning>>

Produce == Display \/ StartAudio \/ HardwareDone \/ ReportSegment \/ NextSegment
Quiescent == ~InAnswer /\ audioQ = <<>> /\ endQ = <<>> /\ UNCHANGED vars
Next == Produce \/ TakeAudio \/ TakeEnd \/ Stop \/ NewTurn \/ Quiescent
Spec == Init /\ [][Next]_vars /\ WF_vars(Produce) /\ WF_vars(TakeAudio) /\ WF_vars(TakeEnd)

FollowupHasEvidence ==
    runtime.mode = "Followup" =>
        (segment = last /\ sawEnd /\ media.reported /\ media.displayed /\
         (~narrated \/ media.drained))

StoppedHasNoPlayback ==
    runtime.mode = "Stopped" => (runtime.playback = 0 /\ ~runtime.endPending)

\* If runtime still owns playback, only consuming its own hardware DONE can
\* make the transition into Followup, even after an authentic seal arrived.
WaitForRegisteredPlayback ==
    [][(runtime.mode # "Followup" /\ runtime'.mode = "Followup" /\ runtime.playback # 0)
       => (audioQ # <<>> /\ Len(audioQ') < Len(audioQ) /\
           Head(audioQ) = <<turn, runtime.playback>>)]_vars

OldReceiptsAreInert ==
    [][((audioQ # <<>> /\ Len(audioQ') < Len(audioQ) /\
         (~InAnswer \/ Head(audioQ) # <<turn, runtime.playback>>)) \/
        (endQ # <<>> /\ Len(endQ') < Len(endQ) /\ (~InAnswer \/ Head(endQ) # turn)))
       => UNCHANGED runtime]_vars

\* Fair progress of the collector and both consumers; user input is optional.
FinishesUnlessInterrupted ==
    \A t \in 1..MaxTurns :
        (turn = t /\ InAnswer) ~> (turn # t \/ runtime.mode \in {"Followup", "Stopped"})

\* A delegated explanation still needs the final segment's hardware receipt.
\* Closing that foreground does not finish the background task.
DelegatedKeepsTheTask == delegated => taskRunning
DelegatedReleaseUsesTheFinalReceipt ==
    delegated /\ runtime.mode = "Followup" =>
        (segment = last /\ sawEnd /\ media.reported /\ media.displayed /\
         (~narrated \/ media.drained))
=============================================================================
