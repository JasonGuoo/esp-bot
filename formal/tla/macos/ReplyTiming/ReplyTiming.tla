------------------------------ MODULE ReplyTiming ------------------------------
EXTENDS Naturals, Sequences
VARIABLES phase, identity, eligible, slow, ackUsed, ready, spoken, output
vars == <<phase, identity, eligible, slow, ackUsed, ready, spoken, output>>

\* eligible abstracts negotiated voice, the wake/cooldown checks and usable ACK audio.
Init == /\ phase = "Input" /\ identity = "Pending" /\ eligible \in BOOLEAN
        /\ slow = FALSE /\ ackUsed = FALSE /\ ready = {} /\ spoken = 0 /\ output = <<>>

Recognize == /\ phase = "Input" /\ phase' = "Thinking"
             /\ UNCHANGED <<identity, eligible, slow, ackUsed, ready, spoken, output>>

Identify == /\ identity = "Pending" /\ identity' = "Labeled"
            /\ UNCHANGED <<phase, eligible, slow, ackUsed, ready, spoken, output>>

Timer == /\ phase = "Thinking" /\ ~slow /\ slow' = TRUE
         /\ UNCHANGED <<phase, identity, eligible, ackUsed, ready, spoken, output>>

Acknowledge == /\ phase = "Thinking" /\ eligible /\ slow /\ ~ackUsed
               /\ ackUsed' = TRUE /\ output' = Append(output, "Ack")
               /\ UNCHANGED <<phase, identity, eligible, slow, ready, spoken>>

FirstContent == /\ phase = "Thinking" /\ phase' = "Streaming"
                /\ UNCHANGED <<identity, eligible, slow, ackUsed, ready, spoken, output>>

\* Two TTS jobs can become ready in either order. Only the contiguous prefix is offered.
Prepare(i) == /\ phase = "Streaming" /\ i \notin ready /\ ready' = ready \cup {i}
              /\ UNCHANGED <<phase, identity, eligible, slow, ackUsed, spoken, output>>
Deliver == /\ phase = "Streaming" /\ spoken+1 \in ready
           /\ spoken' = spoken+1
           /\ output' = Append(output, IF spoken' = 1 THEN "First" ELSE "Second")
           /\ phase' = IF spoken' = 2 THEN "Done" ELSE "Streaming"
           /\ UNCHANGED <<identity, eligible, slow, ackUsed, ready>>

Stop == /\ phase \in {"Input", "Thinking", "Streaming"} /\ phase' = "Stopped"
        /\ UNCHANGED <<identity, eligible, slow, ackUsed, ready, spoken, output>>
Terminal == phase \in {"Done", "Stopped"} /\ UNCHANGED vars
Next == Recognize \/ Identify \/ Timer \/ Acknowledge \/ FirstContent
        \/ Prepare(1) \/ Prepare(2) \/ Deliver \/ Stop \/ Terminal
Spec == Init /\ [][Next]_vars /\ WF_vars(Recognize) /\ WF_vars(FirstContent)
             /\ WF_vars(Prepare(1)) /\ WF_vars(Prepare(2)) /\ WF_vars(Deliver)

OrderedSpeech == output \in {<<>>, <<"Ack">>, <<"First">>, <<"Ack","First">>,
                             <<"First","Second">>, <<"Ack","First","Second">>}
AckOnlyWhileWaiting == [][(~ackUsed /\ ackUsed') => (phase = "Thinking" /\ eligible /\ slow)]_vars
IdentityOnlyLabels == [][(identity # identity') => UNCHANGED <<phase, spoken, output>>]_vars
SilentAfterStop == [][(phase = "Stopped") => UNCHANGED output]_vars
\* Healthy ASR, generation and TTS get service. Identity may stay pending forever.
ProgressWithoutIdentity == (phase = "Input") ~> (phase \in {"Done","Stopped"})
================================================================================
