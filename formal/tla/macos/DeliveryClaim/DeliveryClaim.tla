--------------------------- MODULE DeliveryClaim ---------------------------
EXTENDS Naturals, FiniteSets
\* Two gateways may prepare the same frozen result. Only a successful durable
\* revision claim grants speech; preparation and an output lease are insufficient.
CONSTANT Connections
VARIABLES revision, prepared, owner, lease, sent, received, stopped
vars == <<revision, prepared, owner, lease, sent, received, stopped>>

Init ==
    /\ revision = 1
    /\ prepared = [c \in Connections |-> 0]
    /\ owner = 0
    /\ lease = [c \in Connections |-> FALSE]
    /\ sent = {} /\ received = {} /\ stopped = {}

Prepare(c) ==
    /\ prepared[c] = 0 /\ owner = 0 /\ c \notin stopped
    /\ prepared' = [prepared EXCEPT ![c] = revision]
    /\ lease' = [lease EXCEPT ![c] = TRUE]
    /\ UNCHANGED <<revision, owner, sent, received, stopped>>

Claim(c) ==
    /\ prepared[c] = revision /\ owner = 0 /\ c \notin stopped
    /\ revision' = revision + 1 /\ owner' = c
    /\ UNCHANGED <<prepared, lease, sent, received, stopped>>

Lose(c) ==
    /\ prepared[c] > 0 /\ prepared[c] # revision /\ owner # c
    /\ lease' = [lease EXCEPT ![c] = FALSE]
    /\ UNCHANGED <<revision, prepared, owner, sent, received, stopped>>

Send(c) ==
    /\ owner = c /\ lease[c] /\ c \notin stopped
    /\ sent' = sent \cup {c}
    /\ UNCHANGED <<revision, prepared, owner, lease, received, stopped>>

Receipt(c) ==
    /\ c \in sent /\ owner = c /\ lease[c] /\ c \notin stopped
    /\ received' = received \cup {c}
    /\ UNCHANGED <<revision, prepared, owner, lease, sent, stopped>>

Stop(c) ==
    /\ c \notin stopped
    /\ stopped' = stopped \cup {c}
    /\ lease' = [lease EXCEPT ![c] = FALSE]
    /\ UNCHANGED <<revision, prepared, owner, sent, received>>

Next == (\E c \in Connections : Prepare(c) \/ Claim(c) \/ Lose(c) \/ Send(c) \/ Receipt(c) \/ Stop(c))
    \/ UNCHANGED vars
Spec == Init /\ [][Next]_vars
OnlyClaimWinnerSpeaks == sent \subseteq {owner}
AtMostOneSpeaker == Cardinality(sent) <= 1
ReceiptRequiresSpeech == received \subseteq sent
LosingClaimPreservesWinner == owner # 0 /\ owner \notin stopped => lease[owner]
BoundedClaims == revision \in 1..2
=============================================================================
