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
# The mar345 hits the tth arm when it moves with tth low (mymar_in lifts
# tth to 45 before it moves the mar in, align_in moves it out below
# martran 1200): the mar's motors move only with tth >= 83, and tth goes
# below 83 only with martran >= 1200. A motor record's move that breaks
# it is refused; a SPEC move that breaks it is stopped with ^C.
SpecInterlock("$(PORT)", "tth", 83, "martran", 1200, "martran marz marsdd marshut marx ipbsx ipbsy")
SpecCreate("$(PORT)", "$(SPEC)", "$(MPORT)", "sy shold th tth nu scell martran marsdd marshut sx phi marz marx ipbsx ipbsy", "sec icxrd det ic1", 20, 500)

dbLoadRecords("$(SPEC_IOC)/db/spec.db", "P=$(P),PORT=$(PORT)")
dbLoadTemplate("db/bl5a.substitutions", "P=$(P),PORT=$(MPORT),SPORT=$(PORT)")
dbLoadRecords("$(SCALER)/db/scaler.db", "P=$(P),S=scaler1,DTYP=Asyn Scaler,OUT=@asyn($(PORT)),FREQ=10000000")

iocInit()
