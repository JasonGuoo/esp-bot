--------------------------- MODULE CapabilityExecution ---------------------------
\* The coordinator ledger and domain evidence commit independently. Archive
\* expiry is not explicit forgetting. Playback here is the server's permission
\* to select another frame, NOT an instantaneous physical mute of in-flight PCM.
EXTENDS Naturals,FiniteSets
CONSTANT Operations
VARIABLES ledger, evidence, creations, valid, archived, applied, plans,
          owner, permitted, heard, settlements, queryCount
vars == <<ledger,evidence,creations,valid,archived,applied,plans,owner,
          permitted,heard,settlements,queryCount>>
Init == /\ ledger = [o \in Operations |-> "absent"]
        /\ evidence = {} /\ creations = [o \in Operations |-> 0]
        /\ valid = Operations /\ archived = {} /\ applied = {}
        /\ plans = [o \in Operations |-> "absent"]
        /\ owner = [o \in Operations |-> "none"]
        /\ permitted = {} /\ heard = {} /\ settlements = [o \in Operations |-> 0]
        /\ queryCount = 0
Accept(o) == /\ o \in valid /\ o \notin archived /\ ledger[o] = "absent"
             /\ ledger' = [ledger EXCEPT ![o] = "accepted"]
             /\ UNCHANGED <<evidence,creations,valid,archived,applied,plans,owner,permitted,heard,settlements,queryCount>>
Commit(o) == /\ o \in valid /\ o \notin archived /\ ledger[o] = "accepted" /\ creations[o] = 0
             /\ evidence' = evidence \cup {o} /\ creations' = [creations EXCEPT ![o] = 1]
             /\ plans' = [plans EXCEPT ![o] = "pending"]
             /\ UNCHANGED <<ledger,valid,archived,applied,owner,permitted,heard,settlements,queryCount>>
Reply(o) == /\ o \in evidence /\ ledger[o] \in {"accepted","unknown"}
            /\ ledger' = [ledger EXCEPT ![o] = "applied"] /\ applied' = applied \cup {o}
            /\ UNCHANGED <<evidence,creations,valid,archived,plans,owner,permitted,heard,settlements,queryCount>>
Crash == /\ ledger' = [o \in Operations |-> IF ledger[o] = "accepted" THEN "unknown" ELSE ledger[o]]
         /\ permitted' = {}
         /\ UNCHANGED <<evidence,creations,valid,archived,applied,plans,owner,heard,settlements,queryCount>>
ExpireArchive(o) == /\ o \notin archived /\ archived' = archived \cup {o}
                    /\ UNCHANGED <<ledger,evidence,creations,valid,applied,plans,owner,permitted,heard,settlements,queryCount>>
Revoke(o) == /\ o \in valid /\ valid' = valid \ {o} /\ evidence' = evidence \ {o}
             /\ plans' = [plans EXCEPT ![o] = IF @ = "pending" THEN "cancelled" ELSE @]
             /\ permitted' = permitted \ {o}
             /\ UNCHANGED <<ledger,creations,archived,applied,owner,heard,settlements,queryCount>>
Cancel(o) == /\ plans[o] = "pending" /\ plans' = [plans EXCEPT ![o] = "cancelled"]
             /\ permitted' = permitted \ {o}
             /\ UNCHANGED <<ledger,evidence,creations,valid,archived,applied,owner,heard,settlements,queryCount>>
Claim(o,d) == /\ plans[o] = "pending" /\ owner[o] = "none"
              /\ owner' = [owner EXCEPT ![o] = d]
              /\ UNCHANGED <<ledger,evidence,creations,valid,archived,applied,plans,permitted,heard,settlements,queryCount>>
Start(o) == /\ plans[o] = "pending" /\ owner[o] # "none" /\ o \in valid
            /\ permitted' = permitted \cup {o}
            /\ UNCHANGED <<ledger,evidence,creations,valid,archived,applied,plans,owner,heard,settlements,queryCount>>
Hear(o) == /\ o \in permitted /\ heard' = heard \cup {o}
           /\ UNCHANGED <<ledger,evidence,creations,valid,archived,applied,plans,owner,permitted,settlements,queryCount>>
Report(o,d) == /\ o \in heard /\ owner[o] = d /\ settlements[o] = 0
               /\ settlements' = [settlements EXCEPT ![o] = 1]
               /\ plans' = [plans EXCEPT ![o] = IF @ = "pending" THEN "spoken" ELSE @]
               /\ permitted' = permitted \ {o}
               /\ UNCHANGED <<ledger,evidence,creations,valid,archived,applied,owner,heard,queryCount>>
ReadQuery == /\ queryCount < 2 /\ queryCount' = queryCount+1
             /\ UNCHANGED <<ledger,evidence,creations,valid,archived,applied,plans,owner,permitted,heard,settlements>>
Next == (\E o \in Operations: Accept(o) \/ Commit(o) \/ Reply(o) \/ Revoke(o) \/ ExpireArchive(o)
                 \/ Cancel(o) \/ Start(o) \/ Hear(o)
                 \/ (\E d \in {"device1","device2"}: Claim(o,d) \/ Report(o,d)))
        \/ Crash \/ ReadQuery
Spec == Init /\ [][Next]_vars /\ WF_vars(ReadQuery)
OneCommitPerIntent == \A o \in Operations: creations[o] \in 0..1
ReceiptRequiresEvidence == applied \subseteq {o \in Operations: creations[o] = 1}
NoBlindEffectRetry == \A o \in Operations: ledger[o] = "unknown" /\ creations[o] = 0 => o \notin evidence
EvidenceAndEffectAreAtomic == evidence \subseteq {o \in Operations: creations[o] = 1}
RevocationDeletesEvidence == evidence \subseteq valid
CancelledOfferCannotStart == permitted \subseteq {o \in valid: plans[o] = "pending"}
ReminderSettledOnce == \A o \in Operations: settlements[o] \in 0..1
ActualSpeechIsRequired == \A o \in Operations: settlements[o] = 1 => o \in heard
ArchiveDoesNotCancelPlan == \A o \in archived \cap valid: creations[o] = 1 => plans[o] # "absent"
ReadQueriesProgressWithoutStorage == <>(queryCount = 2)
================================================================================
