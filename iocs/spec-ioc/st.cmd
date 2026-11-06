#============================================================
# st.cmd — SPEC IOC startup script
#
# Usage:
#   cargo run -p spec-ioc -- st.cmd
#
# SPEC runs in server mode (`spec -S 6510`). The IOC reconnects whenever
# SPEC restarts, and its records are in alarm while SPEC is away.
#============================================================

epicsEnvSet("P",     "SPEC:")
epicsEnvSet("PORT",  "spec")
epicsEnvSet("MPORT", "spec_motor")
epicsEnvSet("SPEC",  "localhost:6510")
# asyn_motor.db, which spec_axis.db includes.
epicsEnvSet("EPICS_DB_INCLUDE_PATH", "$(MOTOR)/db:$(SPEC_IOC)/db")

# SpecCreate(port, host:port, motorPort, "motors", "counters",
#            movingPollPeriod, idlePollPeriod)
# The first counter is SPEC's timer and the second its monitor: the
# scaler's PR1 is the time preset, PR2 the monitor preset.
SpecCreate("$(PORT)", "$(SPEC)", "$(MPORT)", "tth th", "sec mon det", 100, 500)

dbLoadRecords("$(SPEC_IOC)/db/spec.db", "P=$(P),PORT=$(PORT)")
dbLoadTemplate("$(SPEC_IOC)/db/example.substitutions", "P=$(P),PORT=$(MPORT),SPORT=$(PORT)")
dbLoadRecords("$(SCALER)/db/scaler.db", "P=$(P),S=scaler1,DTYP=Asyn Scaler,OUT=@asyn($(PORT)),FREQ=10000000")

iocInit()

# Example:
#   caput -c -S SPEC:Cmd "wa"
#   caget -S SPEC:Cmd:Reply
#   caput SPEC:tth 10
#   caput SPEC:scaler1.TP 1; caput SPEC:scaler1.CNT 1
