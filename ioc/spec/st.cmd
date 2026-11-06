#============================================================
# BL5A SPEC IOC
#
# Run from this directory, with SPEC (or bl5a-sim) in server mode:
#   SPEC=host:port ../../target/release/spec-ioc st.cmd
# SPEC defaults to bl5a-sim's localhost:16510.
#============================================================

epicsEnvSet("P",     "BL5A:SPEC:")
epicsEnvSet("PORT",  "spec")
epicsEnvSet("MPORT", "spec_motor")
epicsEnvSet("SPEC",  "$(SPEC=localhost:16510)")
# asyn_motor.db, which spec_axis.db includes, and spec-ioc's templates.
epicsEnvSet("EPICS_DB_INCLUDE_PATH", "$(MOTOR)/db:$(SPEC_IOC)/db")

# The first counter is the timer (PR1 = time preset), the second the
# monitor (PR2 = monitor preset).
# The 20 ms moving poll matches bl5a-sim's event tick, so a fly scan's
# RBV lags the counts by at most one tick.
SpecCreate("$(PORT)", "$(SPEC)", "$(MPORT)", "sy shold th tth nu scell martran marsdd marshut sx phi", "sec icxrd det ic1", 20, 500)

dbLoadRecords("$(SPEC_IOC)/db/spec.db", "P=$(P),PORT=$(PORT)")
dbLoadTemplate("db/bl5a.substitutions", "P=$(P),PORT=$(MPORT),SPORT=$(PORT)")
dbLoadRecords("$(SCALER)/db/scaler.db", "P=$(P),S=scaler1,DTYP=Asyn Scaler,OUT=@asyn($(PORT)),FREQ=10000000")

iocInit()
