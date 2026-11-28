-------------------------- MODULE CancellationCleanup --------------------------
\* Stop invalidates local output before SQLite cleanup. The database may never
\* become available. A delayed cleanup carries exact old lease IDs, not a query
\* for every lease in the activation (which would revoke the next answer).
EXTENDS Naturals, FiniteSets
CONSTANT Capacity
VARIABLES fence, live, retired, cleanup, jobs, stopRequested, stopped, nextGranted
vars == <<fence,live,retired,cleanup,jobs,stopRequested,stopped,nextGranted>>
Init == /\ fence = 1 /\ live = {1} /\ retired = {} /\ cleanup = {}
        /\ jobs = 0 /\ stopRequested = FALSE /\ stopped = FALSE /\ nextGranted = FALSE
Prepare == /\ jobs < Capacity /\ jobs' = jobs+1
           /\ UNCHANGED <<fence,live,retired,cleanup,stopRequested,stopped,nextGranted>>
FinishPreparation == /\ jobs > 0 /\ jobs' = jobs-1
                     /\ UNCHANGED <<fence,live,retired,cleanup,stopRequested,stopped,nextGranted>>
RequestStop == /\ ~stopRequested /\ stopRequested' = TRUE
               /\ UNCHANGED <<fence,live,retired,cleanup,jobs,stopped,nextGranted>>
Stop == /\ stopRequested /\ ~stopped /\ fence' = 2 /\ stopped' = TRUE
        /\ retired' = {1} /\ cleanup' = {1}
        /\ UNCHANGED <<live,jobs,stopRequested,nextGranted>>
GrantNext == /\ stopped /\ ~nextGranted /\ nextGranted' = TRUE /\ live' = live \cup {2}
             /\ UNCHANGED <<fence,retired,cleanup,jobs,stopRequested,stopped>>
\* No fairness for this action: a database or cleanup may stay blocked.
Clean == /\ cleanup # {} /\ live' = live \ cleanup /\ cleanup' = {}
         /\ UNCHANGED <<fence,retired,jobs,stopRequested,stopped,nextGranted>>
Next == Prepare \/ FinishPreparation \/ RequestStop \/ Stop \/ GrantNext \/ Clean
Spec == Init /\ [][Next]_vars /\ WF_vars(Stop)
BoundedPreparations == jobs \in 0..Capacity
NoRetiredLocalOutput == stopped => fence # 1
CleanupCannotRevokeNewOutput == nextGranted => 2 \in live
StopDoesNotWaitForStorage == stopRequested ~> stopped
================================================================================
