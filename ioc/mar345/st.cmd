#============================================================
# BL5A mar345 IOC (ADmar345) on bl5a-sim's mar345dtb face
#
# Run from this directory:
#   MAR_DATA=/abs/dir/ ../../target/mar345-ioc/bin/mar345-ioc st.cmd
# MARSERVER defaults to bl5a-sim's localhost:5001. MAR_DATA must be an
# absolute, existing directory: the driver sends the path in COMMAND SCAN,
# and bl5a-sim writes the file there from its own working directory.
# spec-ioc holds this host's CA and PVA server ports; give this IOC others
# (EPICS_CAS_SERVER_PORT, EPICS_PVAS_*) and list both servers in clients'
# EPICS_CA_ADDR_LIST.
#============================================================

epicsEnvSet("PREFIX",    "BL5A:MAR345:")
epicsEnvSet("PORT",      "MAR")
epicsEnvSet("MARSERVER", "$(MARSERVER=localhost:5001)")
epicsEnvSet("QSIZE",     "20")
epicsEnvSet("XSIZE",     "3450")
epicsEnvSet("YSIZE",     "3450")
epicsEnvSet("NCHANS",    "2048")
epicsEnvSet("CBUFFS",    "500")
epicsEnvSet("NELEMENTS", "12000000")
epicsEnvSet("EPICS_DB_INCLUDE_PATH", "$(ADCORE)/db")

# Commands and replies are LF-terminated; the driver matches replies with
# no terminator of its own.
drvAsynIPPortConfigure("marServer", "$(MARSERVER)")
asynOctetSetInputEos("marServer", 0, "\n")
asynOctetSetOutputEos("marServer", 0, "\n")

mar345Config("$(PORT)", "marServer", 0, 0)
dbLoadRecords("$(ADMAR345)/db/mar345.template", "P=$(PREFIX),R=cam1:,PORT=$(PORT),ADDR=0,TIMEOUT=1,MARSERVER_PORT=marServer")

NDStdArraysConfigure("Image1", 5, 0, "$(PORT)", 0, 0)
dbLoadRecords("$(ADCORE)/db/NDStdArrays.template", "P=$(PREFIX),R=image1:,PORT=Image1,ADDR=0,TIMEOUT=1,NDARRAY_PORT=$(PORT),TYPE=Int16,FTVL=SHORT,NELEMENTS=$(NELEMENTS)")

< $(ADCORE)/ioc/commonPlugins.cmd

iocInit()

# What the beamline's autosave and mar345setup leave: 345 mm at 100 um, no
# auto erase, single 0 s frames (the exposure is SPEC's uct), and files
# named as exp_condition announces them, %s_%-0.4d.mar3450.
dbpf("$(PREFIX)cam1:ScanSize", "3")
dbpf("$(PREFIX)cam1:ScanResolution", "0")
dbpf("$(PREFIX)cam1:ChangeMode", "1")
dbpf("$(PREFIX)cam1:EraseMode", "0")
dbpf("$(PREFIX)cam1:AcquireTime", "0")
dbpf("$(PREFIX)cam1:AcquirePeriod", "0")
dbpf("$(PREFIX)cam1:NumImages", "1")
dbpf("$(PREFIX)cam1:ImageMode", "0")
dbpf("$(PREFIX)cam1:ArrayCallbacks", "1")
dbpf("$(PREFIX)cam1:FilePath", "$(MAR_DATA)")
dbpf("$(PREFIX)cam1:FileTemplate", "%s%s_%4.4d")
dbpf("$(PREFIX)cam1:FileNumber", "1")
dbpf("$(PREFIX)cam1:AutoIncrement", "1")
# The decoded frame for CA clients.
dbpf("$(PREFIX)image1:EnableCallbacks", "1")
