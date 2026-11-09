# BL5A 자동화 GUI 설계

한 GUI에서 정렬과 mar345 측정을 큐로 돌리고, 측정 프레임을 rsFAI로
적분해 시간·온도 순으로 쌓아 보여준다. 대상은 `5a.zip`의 두 실험이다.

- 이차전지 충방전: `coin_align` → `mymar1`(1회) / `mymar`(Ti분 간격 반복).
  충방전기는 노트북에서 따로 제어한다.
- 박막 온도: `myinsitu` = 온도마다 `settemp` → `align_in.mac` → `mymar_in`.
  온도 컨트롤러(LakeShore 331)는 SPEC 시리얼에서 떼어 EPICS IOC로 옮긴다.
  IOC 전까지는 plan이 SPEC 온도 매크로를 그대로 부른다.

이 두 흐름이 쓰는 것만 만든다. 범용 스캔 화면, 재적분, 두 흐름 밖의
매크로는 넣지 않는다.

하드웨어 없이 전 스택을 돌릴 수 있어야 한다. 시뮬레이션은 **프로토콜
경계에서만** 갈아끼운다(SPEC 서버 TCP, mar345dtb TCP, LakeShore 331의
터미널 서버 TCP 포트). 그 위의 IOC·데몬·적분기·GUI는 빔라인과 같은
바이너리다.

`ref/spec/`에는 빔라인 원본 중 세션 설정 `kappa.config`(bl5a-sim이
읽는다), SPEC 상태 파일의 매크로 정의 추출기 `spec_state_dump.py`, SPEC
배포본 splot의 피크 계산(`splot_DataStatistics.py`)만 둔다. 매크로 정의,
`sixc.config`, SPEC 서버 프로토콜 매뉴얼은 SPEC 저작권과 빔라인 내부
설정이라 저장소에 넣지 않는다. 매크로 정의는 `261104_spec_d.tar.gz`의
상태 파일(`userfiles/bl5a_ttyp#S`)에서
`python3 ref/spec/spec_state_dump.py <상태 파일> > kappa_bl5a_macros.mac`
(전역 변수는 `--globals`)로 뽑고, 매뉴얼은 SPEC에서 `help server`로 본다.
아래의 매크로 동작은 모두 이 덤프에서 읽었다.

## 구성

```
  빔라인                                    시뮬레이션
  SPEC (kappa -S <port>) ─┐                  ┌─ bl5a-sim  (프로세스 하나, world state 하나)
  mar345dtb (:5001) ──────┤  이 경계만 교체  ├─   SPEC 서버 face   :6510
  LakeShore 331 ──────────┘                  └─   mar345dtb face :5001, LakeShore face (TCP)
  (192.168.50.93:4008)
          │
  bl5a-spec-ioc  speca 포트: motor·scaler·명령 레코드           BL5A:SPEC:*
  mar345-ioc     epics-rs-iocs 기존 IOC (빔라인은 운영 중 IOC)  BL5A:MAR345:*
  bl5a-temp-ioc  LakeShore 331 드라이버 (drivers/ip 방식)        BL5A:TC:*
          │ CA
  bl5a-qs        bsrs qs-manager: 디바이스, plan, suspender
          │ 문서 PUB ──┬─ 문서 저장 (bl10c와 같은 mongo_writer)
          │            └─ bl5a-integrator (rsFAI) ── 적분 PUB / REP
          │ JSON-RPC                          │
  GUI (Qt) ───────────────────────────────────┘
```

GUI가 꺼져도 큐와 적분은 계속 돈다(bl10c/bl7c와 같은 구조). 이어지는
단계(온도 → 정렬 → 측정)는 GUI가 아니라 plan 하나 안에서 묶는다.

## 빔라인 (SPEC `kappa` 세션)

5a 매크로는 `kappa` 세션에서 돈다(`sixc`에는 tth가 없다). `th/chi/phi`는
kappa 의사모터다.

- **모터**: tth와 martran·marsdd·marz·marshut·marx·ipbsx·ipbsy는 SPEC
  config에서 이미 EPICS 모터(`EPICS_M2`)다. sx·sy·shold·kth·kap·kphi·nu·mu·
  scell·슬릿은 OMS VME, 모노는 KOSMOS다. 이동은 계속 SPEC을 거친다 — kappa
  기하와 `mar_ip7`의 가드가 SPEC에 있다.
- **카운터**: sec(소프트웨어 타이머), ic1·ic2·det·icxrd = VSC16 1–4,
  ringI = `BL5A:userCalcOut1.VAL`, pd = `BL5A:EM1:Dmm_raw.VAL`,
  att = `BL5A:userCalcOut2.VAL`, idet·cdet = 매크로 카운터. 5a 매크로는
  `counters 2 3`(정렬·측정) / `counters 2 5`(코인셀)로 모니터를 icxrd,
  검출기를 det / ic1로 둔다. `user_postcount`가 `CRDG? A`를
  `BL5A:userCalcOut2.A`에 넣으므로 `plotselect att; loopscan`은 온도 기록이다.
- **mar345**: 빔라인도 이미 `BL5A:MAR345:cam1:`이다. 셔터는 검출기 IOC가
  아니라 PFCU1의 날 3이다.
- **시리얼**: 모두 터미널 서버 192.168.50.93, 9600 baud. SPEC이 포트를 쥔다.

| SPEC ser | TCP | 장치 | 매크로 |
| --- | --- | --- | --- |
| 3 | 4001 | PFCU0 | `fi`, `atten` 하위 4비트 |
| 4 | 4002 | PFCU1 | mar345 셔터, `fil`, `atten` 16 이상 |
| 5 | 4006 | 이온챔버 증폭기 (SCPI `:CONF0:CURR:RANG`/`OFFS`) | `ic_range`, `ic_offset` |
| 8 | 4005 | mon1 검출기 (`:SOUR0:VOLT`, `:INP0:GAIN`, `:SENS0:SCA:*`) | `mon1_setup` |
| 9 | 4008 | LakeShore 331 | `temperature.mac`의 온도 매크로 전부, `user_postcount`, `Ftail`, `settemp2` |

**매크로 동작** — sim과 plan이 맞춰야 하는 것:

- `mar345expose(filename, exposure)`: `cam1:FileName` put → 셔터 열기
  (ser 4 `!PFCU00 I0000`, `R1234`) → `uct(exposure)`(429 s를 넘으면 429 s
  조각으로, 음수는 모니터 프리셋) → 셔터 닫기(`I0030`, `R1204`) →
  `exp_condition`(로그) → `cam1:Acquire=1`, `DetectorState_RBV`가
  Scanning인 동안 대기 → `FileNumber−1`.
- `mar345erase`: `cam1:Erase=1`, `DetectorState_RBV`가 Erasing인 동안 대기.
- `marstart2`(align_in의 빔 확인): `ic_offset 0` 뒤 셔터 열기 → `uctn 3` →
  닫기를 icxrd > 100이 될 때까지 반복한다. 판에 빔이 닿으므로 align_in은
  끝에서 erase한다.
- `cscan m a b t [s]`: a로 이동 → 속도를 |b−a|·step_size/t로 바꾸고 →
  `count_em 429`를 건 채 b로 움직이며 s초마다 카운터와 위치를 읽는다. 한
  점은 직전 샘플 이후의 증가분(순간값 카운터는 그대로)이고 위치는 두 샘플의
  중점이며, 첫 샘플은 버린다. 끝나면 속도·base_rate를 되돌린다. `clup`은
  현재 위치 기준 상대 cscan 뒤 시작 위치로 돌아간다.
- `CEN`, `pl_COM`, `pl_xMAX`, `pl_FWHM`은 마지막 스캔의 `plotselect`
  카운터로 계산하는 매크로다(`CEN` = `pl_CFWHM`, FWHM 구간의 중심;
  `array_op("cfwhm"|"com"|"fwhm")`, `pl_xMAX` = 최대점의 x).
- `mar_ip7 martran marsdd marz marshut marx ipbsx ipbsy`: UPWARD
  기하(`_1` ≥ 0)에서 tth < 80, DOWNWARD(≤ 0)에서 tth > −80이면 거부한다.
  scell=0과 일곱 축을 한 번에 움직인다. `mar_ip_mode off`는 같은 가드로
  `mar_ip 1280 835 <marz> 0`이다. 인자 없는 `mar_ip`(coin_align 끝)는 사용법과
  위치만 출력한다. `shut n`은 marshut 이동이다.
- `ds_wid v h` / `ss_wid v h`: 슬릿 hs5* / hs4*를 0 중심 대칭으로 연다.
  `all6`은 tth th chi phi nu mu를 한 번에 움직인다. `setkappa t`는 kappa
  기울기(`g_kappa`)를 정한다.
- `atte n`: 하위 4비트는 PFCU0, 16–95는 PFCU1, 그 뒤 monE 구간으로
  `ic_range`를 맞춘다. mar 셔터를 열고 닫을 때마다 PFCU1 날 1·2·4를 빼므로
  PFCU1 쪽 감쇠는 노출 중에 남지 않는다.
- 온도: kappa 상태의 `settemp`는 한계 검사(`T_LO_SP` −100, `T_HI_SP` 100)
  뒤 `TEMP_SP`만 바꾸고 빈 `_settemp`를 부르는 standard.mac 판이고,
  `set_ramp`·`read_ramp`·`changec`·`readc`는 없고 `DELTA_K`도 없다(0이면
  `changec`는 읽음이 설정점과 정확히 같아질 때까지 돈다). `myinsitu`가
  `temperature.mac`을 `qdo`해서 이것들을 정의한다.
  그 파일은 아카이브에 없고 같은 매크로가 `sixc` 상태에 남아 있다.
  `settemp T` = `SETP 1,T`;
  `set_ramp r` = `RAMP 1,1,r`(K/min); `readc` = `CRDG? A`(°C);
  `changec T` = settemp 뒤 `CRDG? A`가 |T − SP| ≤ `DELTA_K`(sixc 0.3)가 될
  때까지 대기; `tc_range n` = `RANGE n`(응답을 ser 7에서 읽는 결함);
  `set_pid` = `PID 1,P,I,D`. kappa에서는 `Ftail`이 `KRDG? A`를 스캔 파일에
  남긴다(`user_postcount`는 위 카운터 항목).

## bl5a-sim

world state를 한 프로세스가 소유한다: SPEC 니모닉별 모터 위치, 빔(세기,
링 감쇠, PFCU0/1 날 — 필터·감쇠기·mar 셔터, 슬릿), 시료(코인셀 창 / 기판
위 박막), 온도(램프 + 1차 지연), mar345(erase/scan 상태). 세 face는 같은
state를 읽고 쓴다. 그래서 셔터·필터·온도가 카운트와 이미지에 일관되게
반영된다.

**SPEC face** — 서버 프로토콜 v4(132바이트 헤더, SPEC `help server`).
speca가 쓰는 범위:
`motor/<mne>/{position,move_done,low_limit,high_limit,low_lim_hit,high_lim_hit}`
등록·이벤트, `motor/<mne>/start_one`, `motor/../{prestart_all,start_all,abort_all}`,
`scaler/.all./count`, `scaler/<mne>/value`(카운트 중 주기 이벤트 포함).
여기에 `motor/<mne>/{slew_rate,base_rate}`(읽기·쓰기), `SV_CMD(_WITH_RETURN)`,
`SV_ABORT`, `status/ready`, `output/tty`를 더한다. 모터는 slew_rate·base_rate·
가속으로 움직여서, fly 스캔 중 위치와 카운트가 시간에 따라 실기와 같은
꼴로 변한다. 명령은 bl5a-qs의 plan이 보내는 것만 받는다:
- `set`(set_position), `counters`(모니터 프리셋을 세는 채널).
- PFCU·증폭기: `fi`, `fil`, `atte`, `ic_range`, `ic_offset`,
  `mar345openshutter`, `mar345closeshutter` — 날 상태와 증폭 범위를 world
  state에 반영한다.
- 기하: `all6`, `ds_wid`/`ss_wid`(빔 크기), `mar_ip7`·`mar_ip_mode off`(가드
  포함), `setkappa`(가드가 보는 기울기).
- `marstart2`(셔터 + 3 s 카운트 반복), `mar345expose` — 후자는 SPEC처럼
  CA로 mar345-ioc의 `cam1:*`를 쓰므로 검출기 경로는 빔라인과 같다.
- plan이 나눠 보내는 노출의 SPEC 쪽: `exp_condition`(파일 줄만),
  `mar345readout`(`file_no`), 이를 위한 `local a, b`와 `name=값`(문자열은
  따옴표). `local`은 명령 하나가 끝날 때 전역 값을 되돌린다.
- `mon1_setup`, `LF`, `autofil`은 받기만 하고 state를 바꾸지 않는다.
- `cscan`·`clup`: 위 매크로를 그대로 옮긴다(검사, 속도 변경, 샘플 증가분,
  중점, 첫 샘플 버림, `scan_data`). 속도·base_rate 복구는 정상 종료·에러·
  `SV_ABORT` 어느 쪽으로 끝나도 한다(`cleanup_once`). 점은
  `var/{NPTS,SCAN_D,SCAN_COLS}`로 읽힌다: SCAN_D는 1024행씩 자라는 double
  배열이고 NPTS 뒤의 행은 앞선 스캔의 값을 그대로 둔다.

모르는 매크로에는 에러로 답해서, 모델링 안 된 매크로에 기대는 plan이
sim에서 눈에 띄게 실패하게 한다.

**카운터 물리** — 정렬 plan이 수렴할 정답이 있어야 한다.
- direct beam: tth·nu에 대한 가우시안(폭은 ds_wid/ss_wid)
- 박막 half-cut: shold 경계에서 erf 투과, th 기울기에 따른 가림
- specular: `uan 1.2 0.6` 상태에서 th 로킹 피크
- 코인셀: sy·shold에 대한 원형 창 투과(ic1)

시료의 참 높이·각도 오프셋은 sim 시작 때 무작위로 정하고 sim 전용 PV로
공개한다. 정렬 plan의 결과를 이 값과 비교해 검증한다.

**mar345dtb face** — `COMMAND CHANGE/SCAN/ERASE/SHUTTER OPEN|CLOSE`에
답하고 `SCAN_DATA    Ended o.k.`(공백 4칸, `protocol.rs`)로 끝낸다. SCAN은
`.mar<size>` 파일을 쓴다. 이미지는 PONI(rsfai-geometry)와 시료 d-spacing으로
그린 분말 링이다. d-spacing은 시료 상태에 따라 움직인다(온도 → 열팽창·상전이,
전지 → 시간에 따른 리튬화 이동). 세기는 마지막 erase/scan 이후 PFCU1
날 3이 빠져 있던 동안의 빔(필터 반영) 적분에 비례한다 — 실기처럼 판은
셔터가 열린 시간만큼 쌓인다.
pck 인코더는 epics-rs-iocs `mar345::pck::get_pck`의 역으로 작성하고, 왕복
일치로 검증한다(C `mar3xx_pck.c`는 로컬에 없음).

**LakeShore face** — 터미널 서버 포트처럼 TCP로 331 명령을 받는다(줄 끝
CR LF): `SETP`, `SETP?`, `RAMP`, `RAMP?`, `RANGE`, `RANGE?`, `CSET?`,
`CRDG?`, `KRDG?`. temp-ioc 전까지는 SPEC face의 ser 9만 받는다: loop 1의
`SETP 1,T`와 `RAMP 1,on,r`(K/min). 시료 온도는 램프 중인 설정점을 1차
지연(10 s, 측정값 아님)으로 따라가고 가열·냉각이 같다. att 카운터와
`CRDG? A`가 이 값이다. `RAMP? A`의 응답 형식은 모르므로 만들지 않는다.
SPEC face는 sixc판 온도 매크로를 흉내 낸다: `qdo`(myinsitu의
temperature.mac 경로만; 그 전 `settemp`는 kappa의 standard 판이고
`set_ramp`·`read_ramp`·`changec`는 정의 안 됨 에러), `settemp`, `set_ramp`,
`read_ramp`, `changec`, 그리고 standard.mac의 `loopscan n [t [sleep]]`.
SPEC 데이터 파일은 흉내 내지 않으므로 `#Temp:` 줄과 loopscan 점은
`BL5A:SPEC:Log`(tty)에만 남는다.

## bl5a-spec-ioc (speca 포트)

- **SpecSession**: TCP 연결의 유일한 소유자. `read_exact`로 프레이밍하고,
  serial을 증가시켜 응답을 요청에 짝짓는다. SPEC이 재시작하면 재연결하고
  재등록하며, 끊긴 동안 레코드는 COMM/INVALID다.
- **모터**: motor-rs motor 레코드 + `AsynMotor`(asyn-rs `interfaces/motor.rs`)
  구현 `SpecAxis`. `move_absolute` → `start_one`, poll은 이벤트 캐시에서
  읽는다. `start_one`은 SPEC 안에서 `{get_angles;A[mne]=data;move_em;}`가
  되므로 kappa 의사모터 th도 같은 길로 움직인다. 여러 축을 한 번에 옮기는
  매크로의 `A[...]=…; move_all`은 `prestart_all` → `start_one`… →
  `start_all`이다. 축은 두 흐름이 움직이는 sy, shold, th, tth, nu, scell,
  martran, marsdd, marshut과, 인터락이 지켜보는 marz, marx, ipbsx, ipbsy만
  둔다.
- **충돌 인터락**: mar345는 tth가 낮을 때 움직이면 tth 암에 부딪힌다.
  `SpecInterlock("spec", "tth", 40, "martran", 1200, "martran marz marsdd
  marshut marx ipbsx ipbsy")`: mar 모터는 tth ≥ 40이고 tth가 40 아래로
  가는 중이 아닐 때만 움직이고, tth는 martran ≥ 1200이고 mar 모터가 멈춰
  있을 때만 40 아래로 간다. 기준은 매크로에서 왔다(mymar_in은 tth < 40이면
  45로 올린 뒤 tth 81과 함께 martran 25로 넣고, tth 42와 함께 1234로 뺀 뒤
  tth 1로 내리고, align_in은 martran < 1200이면 `mar_ip_mode off`를
  부른다). motor 레코드의 이동은 상태 락 아래에서 거절한다(qs의 `umv tth 81 martran 25`는 tth 이동을 먼저 보내므로 목표가
  40 이상인 tth 이동 중의 mar 이동은 통과한다). 거절된 축은
  `Motion::Refused`가 되어 다음 이동이나 받아들여진 요청까지 problem(MSTA
  RA_PROBLEM → STATE/MAJOR)을 보이고, 그 비트는 요청의 DONE=0과 함께
  나간다. spec_axis.db의 `:Resync`가 그때 SYNC로 VAL을 RBV에 되돌린다
  (TWF·TWR·RLV는 VAL에서 잰다). SPEC 자신의 이동은 거절할
  수 없으므로, 이벤트로 깨진 것을 보면 ^C를 한 번 보낸다(관련 축이 모두
  멈추면 다시 무장). SPEC `mar_ip`의 tth < 80 가드는 그대로 SPEC에 있다.
  `set_position` → SV_CMD `set <mne> <pos>`, stop → speca처럼
  `abort_all`. 같은 위치 이동 때의 DMOV 펄스는 motor 레코드가 처리하므로
  speca의 `motor_put` 토글은 옮기지 않는다. 속도는 `slew_rate`·`base_rate`
  (steps/s = EGU/s × |step_size|)다. 연결 때 SPEC 값을 읽어 VELO·VBAS·VMAX의
  초기값으로 삼고, 값이 바뀔 때만 보낸다. 그래야 motor 레코드가 이동마다
  싣는 속도가 SPEC 자신의 이동 속도를 덮지 않는다.
- **카운터**: scaler-rs scaler 레코드 + `ScalerDriver` 구현. `arm`이
  `scaler/.all./count <preset>`를 보낸다(시간 프리셋은 양수, 모니터 프리셋은
  SPEC `ct`처럼 음수 — `Texpo = -1e5`). `done`은 count 이벤트 1→0 뒤
  `scaler/<mne>/value`를 모두 읽고 나서 세운다. 채널은 두 흐름이 읽는
  sec, icxrd(모니터), det, ic1만 둔다.
  429 s를 넘는 시간 프리셋은 거절한다. 매크로처럼 plan이 나눠 센다.
  카운트 중 `scaler/<mne>/value` 이벤트는 채널마다 I/O Intr ai
  (`BL5A:SPEC:<mne>:Live`)로 받은 시각과 함께 낸다.
- **명령 채널**: `BL5A:SPEC:Cmd`(lso), `Cmd:Busy`, `Cmd:Reply`, `Cmd:Status`
  → `SV_CMD_WITH_RETURN`, `Cmd:Abort` → `SV_ABORT`(^C). SPEC은 직렬로
  실행하므로, 명령이 실행 중일 때 레코드에서 오는 이동·카운트는 busy로
  거절한다. 콘솔에서 친 명령도 막아야 하므로 busy는 `status/ready`로 판단한다.
  이 속성은 극성이 둘이다: 이벤트는 1 = 입력 대기(ready), 읽기는 1 = busy.
- **스캔 명령**: `BL5A:SPEC:ScanCmd`(lso)는 Cmd처럼 명령을 보내고(Busy·
  Abort를 같이 쓴다), 성공하면 Reply를 내기 전에 `var/NPTS`·`var/SCAN_COLS`·
  `var/SCAN_D`를 읽어 SCAN_D의 앞 NPTS행을 `Scan:Data`(double, 행 순서,
  NELM `spec_driver::SCAN_NELM` 65536)에, 열 이름을 줄바꿈으로 이어
  `Scan:Cols`에 낸다. 그래서 put이 끝나면 두 레코드는 그 스캔의 점이다.
  SCAN_COLS가 0, 1, …을 빈틈없이 이름 짓지 않거나 SCAN_D의 열 수·행 수·
  바이트 수가 맞지 않으면 Status가 Failed, Reply가 그 이유이고 두 레코드는
  빈다.
- **콘솔**: `output/tty`를 등록해 SPEC 콘솔 출력과 드라이버 자신의 메시지를
  `BL5A:SPEC:Log`에 낸다. 값은 들어갈 만큼의 최근 줄 전체이고, 줄마다
  `<run>:<seq> ` 번호가 붙는다(run은 드라이버 시작 시각 ms, seq는 그 안에서
  1부터). CA 모니터는 배열의 최신 값만 전하므로(`dbEvent.c`), 한 줄씩 내면
  몰린 줄이 사라진다. GUI 콘솔은 번호로 아직 못 본 줄만 붙이고, 꼬리에서
  밀려난 줄은 개수로, IOC 재시작은 run이 바뀐 것으로 표시한다. NELM은
  `spec_driver::LOG_NELM`(8192)이고 테스트가 둘을 맞춘다.
- **speca에서 옮기지 않을 결함** (`speca/client.py`):
  등록 이름은 `low_lim_hit`/`high_lim_hit`인데 `process`는 `low_limit_hit`을
  비교해서 리밋 히트가 반영되지 않는다. `reader.read(132)`의 부분 읽기를
  `raise` → `except: continue`로 버려서 스트림이 어긋난다. 소켓 EOF에서
  같은 경로로 빈 루프를 돈다. serial이 1234로 고정이라 응답을 짝지을 수 없다.

드라이버(프로토콜, `SpecAxis`, scaler 드라이버)는 epics-rs-iocs와 같은
배치로 이 저장소의 `drivers/spec`, IOC는 `iocs/spec-ioc`에 둔다. 공용
크레이트 `motor-common`은 epics-rs-iocs의 고정 커밋에서 가져온다. BL5A
설정(st.cmd, 니모닉 목록, PV 접두어)은 `ioc/spec`에 둔다. bl5a-sim도 같은
프로토콜 크레이트를 쓴다.

## bl5a-qs (bsrs)

디바이스: SPEC 모터, scaler(시간 프리셋 TP, 모니터 프리셋 PR2·G2),
`spec_cmd`(문자열 put → Busy=0 대기 → Status 에러면 실패, `BL5A:SPEC:Busy`로
SPEC이 프롬프트에 돌아오기를 기다림; 스캔은 `ScanCmd`에 put한 뒤
`Scan:Cols`·`Scan:Data`를 읽음), mar345 cam(FileName, FileNumber,
Acquire, Acquire_RBV, ArrayCounter_RBV, Erase, DetectorState_RBV,
FullFileName_RBV), 온도(`BL5A:TC:*`), suspender용 링
전류(`BL5A:userCalcOut1.VAL`)·셔터.

정렬은 bsrs가 한다. 스텝 스캔은 bsrs가 직접 돌리고, fly 스캔은 샘플
타이밍이 SPEC 안에 있어야 하므로 SPEC의 매크로를 그대로 부른다.
- **스텝 스캔**(`ascan/lup`): 점마다 spec-ioc의 motor 레코드로 이동 →
  scaler 레코드로 시간·모니터 프리셋 카운트 → 읽기.
- **fly 스캔**(`cscan/clup`): `ScanCmd`로 SPEC에 `cscan m a b t s`
  (`clup`)를 보내고, SPEC이 점을 찍을 때마다(`SCAN_PT` 이벤트를 spec-ioc가
  `Scan:Live:Cols`·`Scan:Live:Data`로 옮긴다) 그 점마다 primary 이벤트 하나를
  낸다. 이벤트의 키는 SPEC의 열 이름 그대로(모터, 기하
  모터면 H K L, 쓰인 카운터)이고 값은 SPEC이 낸 그대로(샘플 증가분, 두
  샘플의 중점 위치)다. 점의 x는 0열(모터 이름과 같은지 확인), y는 DET
  열이라 통계가 SPEC의 `CEN`과 같은 배열로 계산된다. 스캔이 끝나면
  `Scan:Cols`·`Scan:Data`의 표가 아직 안 나간 점을 보탠다. 그 표가 이미 낸
  점들로 시작하지 않으면(다른 클라이언트의 스캔 등) 스캔을 실패로 끝낸다.
  실패·중단된 스캔은 이미 낸 점만 남는다. 속도 복구와 `clup`의 복귀는 SPEC 매크로가 한다. plan의
  finalizer는 중단·에러 때 SPEC이 아직 busy면 `Cmd:Abort`를 보내고
  프롬프트로 돌아오기를 기다린다.
- **피크 헬퍼**: bsrs에는 `tune_centroid`만 있으므로 스캔 자신의 읽기값으로
  cen·com·xmax·fwhm을 계산하는 헬퍼를 추가한다. 정의는 SPEC 배포본의
  `splot_DataStatistics.py`를 따른다: 반치는 max/2(배경을 빼지 않음), 최대점에서
  양쪽으로 걸어 처음 반치 이하가 되는 점과 직전 점 사이를 선형 보간, CEN은 두
  반치점의 중간(한쪽이 최대점에 붙으면 다른 쪽), COM = Σxy/Σy, xMAX는 첫
  최대점의 x. SPEC C의 `array_op` 원본은 없으므로 빔라인에서 같은 범위의 SPEC
  스캔 값과 비교해 확인한다.
- **정렬 단계의 검사**(`qs/src/screen.rs`): 통계는 Hampel 필터(자신과 양옆
  2점의 중앙값에서 3σ 넘게 벗어난 점을 그 중앙값으로; σ는 다섯 점의 MAD,
  √중앙값, 스캔 전체 이웃차 MAD 중 큰 것) 뒤의 점으로 계산한다. cscan의 둘째
  샘플은 다른 샘플보다 약 1.7배 오래 세어 CEN·xMAX를 끌고 간다. 그 뒤 25구간
  평균의 변화가 최대의 10% 미만이거나 구간 잡음의 5배 미만이면(보이지 않음),
  피크의 최대가 첫·끝 점이거나 양옆 점이 모두 반치 이하면(한 점 폭), 최대
  구간의 어느 한쪽 구간들이 두 구간 차 잡음의 3배보다 더 내려가지 않으면(끝
  점의 잡음이 끝 밖의 피크를 안쪽 최대로 보이게 함), 에지가
  반치에 닿지 않거나 양쪽에서 닿으면 모터를 스캔 전 위치로 되돌리고 plan을
  실패시킨다. half-cut shold 단계는 CEN 대신 최대와 바닥의 중간 교차점(EDGE)으로
  간다. 바닥은 최저 구간에서 잡음 안에 드는 구간이 3개 이상이면 스캔 최소값,
  아니면(경사에서 끝난 스캔) 0이다. 딥(dip)은 필터 뒤 점을 뒤집어(최대 − 각
  점) 피크와 같은 검사와 통계를 쓰고, 보이는지는 뒤집기 전 점으로 본다. 에지의
  CEN은 교차점과 평탄부의 잡음 최대점의 중간이라, 빔라인에서 같은 에지를 네 번
  스캔했을 때 CEN은 0.24, 교차점은 0.007 안에서 움직였다.
- `umv m CEN; set m 0`은 motor 레코드 이동과 `set_position`(SV_CMD `set`)이다.
- **데이터 폴더**: 큐 아이템 하나가 폴더 하나 `<root>/<series>/`를 갖는다.
  폴더 이름은 아이템의 `md.series`, 없으면 plan 이름이고, root는 아이템의
  `md.data_root`, 없으면 bl5a-qs의 `--data-root`다. GUI는 Data 상자의 폴더를
  큐에 넣는 아이템마다 `md.data_root`로 싣는다: 큐가 데이터가 갈 곳을 보여
  주고, qs는 따로 모드를 갖지 않는다. plan run이 시작할 때 폴더를 만들고
  start 문서에 `data_dir`로 싣는다. 프레임(mar345-ioc `FilePath`를 노출마다
  그 폴더로 두고 `FilePathExists_RBV`를 확인), `<series>.spec`, integrator의
  스택이 모두 그 폴더에 간다.
  폴더 경로는 공백·제어 문자 없이 200바이트 이하다(`Place::of`가 검사하고,
  `--data-root`는 시작할 때, GUI는 큐에 넣기 전에 같은 규칙으로 거른다).
  mar345dtb는 프레임 경로를 `COMMAND SCAN <path>` 줄의 한 단어로 받고,
  ADmar345는 이 줄을 256바이트 버퍼로 보낸다: 공백이 있으면 줄이 갈라져
  프레임이 타임아웃되고, 길면 경로가 소리 없이 잘린다.
- **SPEC 데이터 파일**: SPEC은 서버로 돌아 이 스캔들을 파일에 남기지 않으므로
  bl5a-qs가 아이템 폴더의 `<series>.spec`에 모든 스캔(스텝, fly)을 SPEC 형식으로
  덧붙인다.
  스캔 run의 start md `spec_head`에 `HEADING`(lup은 절대 끝점의 `ascan`, clup은
  `cscan`), `_ctime`, Epoch 여부를 싣고, environment hook이 RunEngine 문서를
  구독해 primary 스트림이 열릴 때 `#S`(번호는 파일의 마지막 `#S` 다음)·`#D`·
  `#T`·`#N`·`#L`을, 이벤트마다 한 행을, 성공 아닌 stop에 `_scanabort`의 `#C …
  Scan aborted after N points.`를 쓴다. 첫 점 전에 끝난 스캔은 번호를 받지
  않는다. 열은 모터, H K L, Epoch(스텝만: fly 점의 이벤트 시각은 SPEC이 찍은
  시각이 아니라 bl5a-qs에 도착한 시각이다), 나머지 키 이름순, 검출기 마지막이다. 라벨은
  니모닉이고 `#O`·`#P`·`#G`·`#Q`는 없다(문서에 모터 표와 기하가 없다).

큐 아이템은 plan 셋뿐이다. 스캔과 노출은 그 안의 조각이고 따로 큐에 넣지
않는다.

| plan | 대응 매크로 |
| --- | --- |
| `align(blocks, sequence)` | coin_align, align_in.mac(정렬 순서로) |
| `mar_series(name, ti, texpo, n, geometry)` | mymar1(n=1), mymar |
| `insitu_step(T, ramp, wait, soak, blocks, sequence, th_list, texpo, name)` | myinsitu의 온도 한 칸 + mymar_in |

정렬은 블록 목록(`qs/src/plans/blocks.rs`)이다. 블록은 scan(type
ascan·lup·cscan·clup, motor, start, finish, 스텝이면 time(점당 초)이나
monitor(점당 SPEC MON인 icxrd 카운트; SPEC처럼 음의 count time으로 쓰인다)
중 하나와 intervals·settle, fly면 time(이동 전체 초)과 sleep, counter, pick CEN·COM·xMAX·EDGE, dip(참이면 딥의 CEN·COM·xMAX,
EDGE와는 함께 못 씀), set), move(`umv`할 [motor,
pos] 쌍), spec(명령 한 줄), wait(초), erase(mar345 판 지우기), loop(제
`blocks`를 순서대로 `times`번; loop 안에 loop도 된다) 여섯 가지이고,
어느 블록이든 `when`({motor, op `<`·`>`, value}: 그 조건일 때만 실행; loop는
시작 전에 한 번)과 `note`를 가질 수 있다. 오류는 블록을 자리로 부른다
("block 4 (loop) block 2 (scan): …"). 모르는 필드, 범위 밖 값은 GUI가
큐에 넣기 전에 거절하고, bl5a-qs도 plan을 만들 때 거절한다.
블록을 다 돌면 끝 위치(sx, sy, shold, th, phi)를 "final" 이벤트로 남긴다.
GUI가 블록을 아이템에 그대로 실어 보내므로 큐에 넣은 뒤 순서를 고쳐도 그
아이템은 바뀌지 않는다. `sequence`는 기록용 이름이다.

정렬 순서는 GUI의 `gui/bl5a_gui/templates/`에 매크로 둘(Coin cell =
coin_align, Thin film = align_in.mac)이 읽기 전용으로 있고, 이용자 것은
`<데이터 폴더>/alignments/<이용자>/<이름>.json`(`{"blocks": [...]}`)에
저장한다. GUI는 저장·읽기 때 bl5a-qs와 같은 규칙으로 검사한다
(`items.block`); bl5a-qs 테스트가 두 템플릿을 그대로 읽어 본다.

아래에서 fly의 괄호는 (이동 시간, 샘플 간격), 스텝의 괄호는 (구간 수, 점당
시간)이다.
- Coin cell: marshut 0 → `mon1_setup 1050 100 300 3.0 1.0` → `setkappa 50`
  → `counters 2 5`, `fi 1 0 0 0`, `fil 0 0 0 0` → 상대 fly sy −2.5…2.5
  (12.5 s, 0.1 s) → COM, 상대 fly shold −2.0…2.0(10.5 s, 0.1 s) → COM →
  `mar345closeshutter` → erase → marshut 340. 검출기는 ic1이다.
- `mar_series`: `mar345closeshutter`, scell 0, `counters 2 3`, `fi 0 1 0 0`,
  `ic_range 4`, `mar_ip7 <geometry>`, scell 0 뒤 n 프레임. geometry는 mymar의
  세 벌이다: Standard `-85 435 100 330 50 1.2 -6.8`(매크로 기본), High Q Asymm
  `-85 435 110 330 80 1.2 -6.8`, Low Q Symmetric `160 435 36 345 54 1.2 -6.8`.
  i번째 프레임은 t0 + i·Ti에 시작한다(매크로와 같이 밀리지 않고, 넘치면 다음
  경계로). 정지 요청은 프레임 사이에서 먹는다.
- Thin film: align_in.mac 순서 그대로다. 준비는 `ds_wid 0.5 1.0`,
  `ss_wid 1.0 6.0`, `LF nu`, `autofil 0`, martran < 1200이면
  `mar_ip_mode off`, tth 1·th 0, `atte 0`, `marstart2`, `atte 11`,
  `counters 2 3`, scell 5, `ic_range 4`, `ic_offset 1`,
  `all6 0 0 89.7 0 0 0.0`, shold −3.0이다. 검출기는 det이다.
  - direct beam: 스텝 tth −0.04…0.04(24구간, 0.3 s) → CEN, set 0; nu
    −0.10…0.10(16구간, 0.3 s) → CEN, set 0.
  - half-cut: fly shold −3.0…−1.5(5 s, 0.05 s) → EDGE; 상대 fly th ±1.0(5 s,
    0.03 s) → xMAX, set 0; 상대 fly shold 0.3…−0.3(2 s, 0.05 s) → EDGE; 상대 fly
    th ±2.0(10 s, 0.03 s) → CEN; 상대 스텝 shold ±0.3(18구간, 0.3 s) → EDGE; th
    ±0.6(30구간, 0.5 s) → CEN, set 0; shold ±0.15(20구간, 0.5 s) → EDGE.
  - specular: tth 1.2·th 0.6, `atte 4`, 상대 스텝 th ±0.2(120구간, 0.5 s) →
    xMAX, set th 0.6.
  - 끝: `mar345closeshutter`, erase. 단계마다 이전·이후 위치, CEN/FWHM과
    필터가 바꾼 점 수(`rejected`)를 이벤트로 낸다.
- `insitu_step`: T나 ramp가 있으면 `qdo` temperature.mac(myinsitu와 같은
  경로) → `set_ramp ramp` → wait이면 `{local DELTA_K; DELTA_K=tol; changec
  T}`로 |T − 읽음| ≤ 허용오차(기본 0.3 °C, sixc `DELTA_K`)까지 대기(kappa에
  `DELTA_K`가 없어 local로 준다), 아니면 myinsitu 첫 칸처럼 `settemp T` →
  soak = (n, t)이면 `loopscan n t`(온도는 SPEC이 att 카운터와 데이터 파일에
  남긴다; `plotselect`는 보내지 않는다) → blocks가 있으면 그 정렬 → 측정(mymar_in):
  - tth < 40이면 셔터를 닫고 tth 45·th 0 뒤 tth 81, th 0, scell 0, martran 25,
    marsdd 440, marshut 340을 한 번에.
  - tth 81·th 0, `counters 2 3`, `fi 1 0 0 0`, `ic_range 5`, `ic_offset 0`,
    `mar_ip7 25 440 42.4 340 81.8 1.2 -6.8`, scell 0.9.
  - th 목록(기본 −0.05, 0.11, 0.13, 0.15)마다 노출. texpo는 프레임당 값을
    그대로 받는다(매크로의 0.7·Texpo, 기본 −7e4 모니터).
  - `ic_offset 1`, `fi 1 0 1 1`, tth 42, th 0, martran 1234, marsdd 835,
    marshut 0, scell 6을 한 번에, 그 뒤 tth 1·th 0.

  냉각은 같은 plan이다: T 25, ramp 25, wait 없음, soak (3600, t), 정렬·노출
  없음(myinsitu 끝의 `set_ramp 25; settemp 025; plotselect att;
  loopscan 3600`). 온도는 temp-ioc(7단계)가 생기면 `BL5A:TC:*`로 옮기고,
  단위는 매크로처럼 °C다. 중단하면 컨트롤러 설정점은 마지막 값에 남는다.
- 노출 조각: `mar345expose`를 단계로 나눠 plan이 순서를 쥔다. 셔터만 SPEC
  시리얼(PFCU1)에 있으므로 SPEC 명령으로 둔다: `cam1:FileName` put →
  `mar345openshutter` → scaler1 카운트(429 s 조각, 음수는 PR2 모니터
  프리셋 한 번; 0 s 나머지는 scaler가 거절하므로 건너뜀) →
  `mar345closeshutter` → `{local filename, exposure; filename="…";
  exposure=…; exp_condition}`(SPEC 데이터 파일의 `#MAR345`·`#Beam_cond1`
  줄) → `cam1:Acquire`, Acquire_RBV가 1에서 0이 되고 ArrayCounter_RBV가
  오를 때까지 대기(Acquire는 bo라 put 완료가 프레임 끝이 아니다) →
  `mar345readout("…")`(SPEC의 `file_no`). 끝나면 scaler의 마지막 카운트,
  FileNumber, FullFileName_RBV를 이벤트에 싣는다. 어떻게 끝나든 plan의
  finalizer가 실행 중 SPEC 명령을 ^C로, 카운트를 CNT=0으로 멈추고 SPEC이
  준비되면 `mar345closeshutter`를 보낸다. 이벤트에는 파일 경로와 함께 mon,
  sec, 온도 RBV/SP, th, shold, 필터 상태를 담는다. erase는 SPEC을 거치지 않고
  `cam1:Erase`와 `DetectorState_RBV`로 직접 한다.
- SPEC으로 보내는 명령(`spec_cmd`)은 `fi`, `fil`, `atte`, `ic_range`,
  `ic_offset`, `mon1_setup`, `marstart2`, `mar345openshutter`,
  `mar345closeshutter`, `exp_condition`, `mar345readout`, `mar_ip7`,
  `mar_ip_mode`, `ds_wid`, `ss_wid`, `all6`, `setkappa`, `LF`, `autofil`,
  `counters`, 그리고 temp-ioc 전까지 `qdo`, `set_ramp`, `settemp`,
  `changec`, `loopscan`뿐이다. PFCU·증폭기·mon1·mar 셔터·온도는
  SPEC이 시리얼 포트를 쥐고, `mar_ip7`·`mar_ip_mode`는 가드가 매크로에 있고,
  나머지는 SPEC 안의 상태를 바꾼다. `counters`는 모니터 프리셋을 세는 SPEC의
  MON 채널을 정하므로 빠뜨리면 안 된다. 백틱 없는 이동(tth 81·th 0, scell
  0.9 같은 것)은 motor 레코드, 괄호 안의 매크로
  이름은 대응 표기다. `plotselect`는 보내지 않고 plan이 피크를 계산할 채널을
  고른다.
- `disable/enable` 카운터는 대응하지 않는다. plan이 읽을 채널을 명시한다.

## bl5a-temp-ioc (LakeShore 331)

- epics-rs-iocs `drivers/ip`와 같은 모양: octet 포트 위의 asyn 포트
  드라이버, 워커 스레드 하나가 명령을 직렬로 보낸다. 드라이버는
  epics-rs-iocs에 두고 BL5A st.cmd는 이 저장소에 둔다.
- 레코드: 설정점(`SETP 1`), 램프 속도·켜기(`RAMP 1`), 히터 범위(`RANGE`),
  온도 °C(`CRDG? A`)와 K(`KRDG? A`). insitu_step이 쓰는 것에 히터를 켤
  `RANGE`만 더한다.
- **포트 소유자는 하나다.** 두 클라이언트가 같은 줄에 명령을 섞으면 응답
  짝이 깨진다. IOC가 4008을 가져가면 ser 9를 쓰는 SPEC 매크로를 모두
  정리한다. kappa 상태에는 `user_postcount`, `Ftail`, `settemp2`가 있고,
  myinsitu가 `qdo`하는 temperature.mac(sixc 상태 기준)은 온도 매크로 전부가
  ser 9를 쓴다. 규칙은 하나다: 위 레코드로 옮길 수 있는 명령(SETP, RAMP,
  RANGE, CRDG?, KRDG?)만 보내는 매크로는 `epics_get/epics_put("BL5A:TC:…")`로
  바꾸고, 그 밖의 명령(CMODE, PID, TUNEST?, `settemp2`의 임의 문자열)을 보내는
  매크로는 지운다. 카운트·스캔마다 도는 `user_postcount`와 `Ftail`이 바뀌므로
  att 카운터, SPEC 데이터 파일, `exp_condition` 로그에 온도가 계속 남는다.
- 설정점 단위는 매크로처럼 °C다. 시작할 때 `CSET? 1`의 단위가 °C가 아니면
  설정점 레코드를 INVALID로 둔다.

## bl5a-integrator (rsFAI)

- 입력은 데몬 문서 PUB다. start 문서의 `series`(없으면 run uid)와
  `data_dir`로 묶고, 이벤트의 mar 파일 경로를 읽어 `mar345::pck::get_pck`로
  디코딩한다. rsfai 적분기는 설정(PONI, npt)과 스캔 크기마다 한 번 만든다.
  마스크는 판의 원 밖이고, q 축(`q_A^-1`)은 0부터 판 가장자리의 q까지를 npt로
  나눈다. 같은 이벤트의 모니터 카운트로 정규화한다.
- 설정은 시작 인자 `--poni`·`--npt`로 시작하고 REP `set`으로 바꾼다. `set`은
  큐에 이미 들어간 프레임 뒤에 적용되고(작업 큐의 `Configure`), 적용될 때
  PUB `config`를 낸다. 재시작하면 시작 인자로 돌아간다.
- `get_pck`는 `i16`을 돌려준다. 적분 전에 32767을 넘는 픽셀(u16 해석)과
  헤더의 high-intensity overflow 레코드를 처리해야 한다. 실제 BL5A 파일로
  확인한다.
- 모드는 시리즈별로 고른다: 1D 방위 적분(전지, 투과), GI/fiber(박막,
  `rsfai-fiber`).
- **불변식: 스택 파일은 integrator만 쓴다.** 스택은 아이템의 데이터 폴더에
  시리즈와 q 축마다 하나다: `<stem>.h5`, q 축이 바뀌면 `<stem>~2.h5`, … .
  행은 최신 스택이 지금의 q 축일 때만 거기 붙고, 아니면 다음 번호를 새로
  시작한다(앞의 축으로 돌아가도 새 번호). 그래서 축이 바뀌어도 기존 스택을
  다시 binning하지 않는다. stem은 ASCII 글자·숫자·`_`·`-`·맨 앞이 아닌 `.`
  밖의 바이트를 `%XX`로 쓴다. 내용은 q, I[N×npt], σ, 그리고 프레임 표(시각,
  run uid, seq, 파일, T_rbv, T_sp, th, mon, 노출, PONI)다. `--root`(절대
  경로) 아래 폴더의 스택은 시작할 때 읽고, 그 밖의 폴더는 첫 행 때 읽는다.
  GUI는 라이브 파일을 열지 않는다. 목록은 REP `stacks`, 기록은 REP
  `history{path, from}`로, 새 행은 PUB `row`로, 행을 못 낸 프레임은 PUB
  `error`로 받는다.
- 꺼져 있던 동안 놓친 프레임은 문서 저장소에서 그 run의 이벤트를 다시
  읽어 메운다.

## GUI

Python Qt(silx.gui.qt + repics). bl7c `pal_tools`의 `bsrs_client.py`,
`QueueView.py`, `Plot1DCustom.py`를 고치지 않고 `gui/bl5a_gui/vendor/`에 둔다.
바탕화면의 **BL5A** 단축키가 `deploy/bl5a-gui`를 거쳐 `gui/run_gui.py`를 띄운다.

```
┌ Ring 399.8 mA │ Shutter OPEN │ SPEC ● │ QS Running │ T 120.0/120.0 °C ↑5/min │ MAR idle │ file ┐
├────────────────┬───────────────────────────────────────────────────┬──────────────────────┤
│ 모터            │ [Scan] [Image] [Stack]                            │ 큐                    │
│ tth   81.000 ◀▶ │                                                   │ ▶ insitu T=120       │
│ th     0.130 ◀▶ │   Stack: q →                                      │   insitu T=150       │
│ shold -2.981 ◀▶ │   y = frame | 시간 | T,  th 필터                    │   insitu T=180       │
│ sy  …          │   ┌─────────── heatmap ───────────┐ ┌ overlay ┐    │   cool → 25          │
│ [Align position]│   │                               │ │         │    ├──────────────────────┤
│ [Measure pos.]  │   └───────────────────────────────┘ └─────────┘    │ 정렬 이력             │
├────────────────┤                                                   │ T   shold   th0       │
│ [정렬]          │                                                   │ 30  -2.981  0.012     │
│[Battery][In-situ]│                                                   │ 60  -2.975  0.010     │
│ (모드별 폼)      │                                                   │                      │
├────────────────┴───────────────────────────────────────────────────┴──────────────────────┤
│ 로그 · SPEC 콘솔(output/tty) · 명령 응답                                                     │
└───────────────────────────────────────────────────────────────────────────────────────────┘
```

- **정렬**: Coin cell·Thin film 버튼이나 Load…(이용자 것)로 순서를 불러오면
  별도의 Blocks 창이 열린다. 거기서 블록을 더하고·고치고·옮기고·지우고,
  이웃한 블록을 loop로 묶어(N회 반복) 이용자 것으로 저장한다. Queue는
  패널에서 한다.
  단계마다 이전→이후 위치, CEN/FWHM을 표시하고 시리즈별 이력(T에 따른
  shold·th0)을 남긴다.
- **전지**: filename(공백 없음, 28자 미만 — 매크로 주석), Ti(분, > 2.5),
  Texpo(초 1–20 또는 음수 모니터), N, geometry 세 벌. 1회(mymar1)/반복(mymar),
  "이번 프레임 후 정지". 시작 전에 빔스톱 확인을 받는다(매크로의
  "Check BEAM STOP").
- **In-situ**: 온도 프로그램 표(T, ramp, wait, soak (n, t), 허용오차 기본
  0.3 °C — wait일 때만 보냄, 정렬 여부, th 목록 기본 −0.05/0.11/0.13/0.15, texpo 기본 −7e4, 이름)
  → 행마다 `insitu_step` 하나를 큐에 넣는다. 마지막 행은 냉각(T 25, ramp 25,
  soak 3600점, 정렬·노출 없음)이다.
- **상태줄**: 링 전류는 `BL5A:userCalcOut1.VAL`(SPEC ringI와 같은 PV).
- **플롯 탭**: Scan(라이브, CEN/COM/MAX 마커), Image(최근 mar 프레임, log,
  PONI 링 오버레이), Stack(q × 프레임 heatmap, y축 선택, th 필터, 선택 행
  overlay/waterfall).
- **Data 상자**: 앞으로 큐에 넣을 아이템의 데이터 root(비면 bl5a-qs의
  `--data-root`)와 mar345-ioc가 지금 쓰는 폴더(`FilePath_RBV`, 안 보이면
  빨간 "missing")를 보인다. root는 QSettings에 남는다.
- **Integration 상자**: integrator의 PONI 파일과 npt를 REP `set`으로
  보낸다. Image 탭의 링은 integrator가 답하면 그 `config`의 PONI를,
  답하기 전에는 `config.PONI`를 쓴다. mask, unit, 모드는 GUI에서 바꾸지
  않는다.
- **MCP 서버**(`gui/run_mcp.py`, `bl5a_gui/mcpserver.py`): Claude Code나
  Codex가 `deploy/bl5a-mcp`로 띄우며, GUI와 같은 일을 하도록 stdio MCP 도구 39개를 연다. 아이템은 폼과
  같은 `items.py`로 만들고, 버튼과 같은 `gate.py` 규칙으로 막고, GUI가 쓰는
  PV(모터 TWV/TWF/TWR, SPEC `Cmd`/`Cmd:Abort`)만 쓴다. qs 상태는 `sync()`
  하나가 읽고, 상태를 보거나 게이트를 거는 도구는 먼저 그것을 부른다.

## 배치 (user systemd)

`deploy/install.sh` 하나로 빌드와 설치를 한다. 상시 도는 spec-ioc,
bl5a-qs, bl5a-integrator(시뮬레이션은 bl5a-sim, mar345-ioc 포함)는
운영자 계정의 user systemd 서비스로, `bl5a.target`이 함께 켜고 끈다.
사람이 직접 띄우는 것은 SPEC, GUI, Claude Code/Codex 셋이고, SPEC과 GUI는
바탕화면 단축키로 띄운다. 설정은 `~/.config/bl5a/bl5a.env` 하나를 서비스
(`EnvironmentFile=`)와 실행기(`sh`)가 같이 읽는다. IOC는 stdin이 끝나면
종료하므로 `deploy/ioc-run`이 FIFO를 stdin으로 물려 둔다. qs는 시작할 때
모든 PV가 연결될 때까지 기다리며, 기다리는 PV를 저널에 남긴다.

## 단계와 검증

1. SPEC 프로토콜 + bl5a-sim SPEC face — 원본 speca(Python)를 sim에 붙여
   이동·카운트가 동작한다(원본 클라이언트를 오라클로 쓴다).
2. spec-ioc — sim 상대로: caput 이동과 DMOV(같은 위치 포함), 시간·모니터
   프리셋 카운트, sim 재시작 후 재연결, 리밋 히트가 반영된다. VELO 변경이
   `slew_rate`로 가고 SPEC 자신의 이동 속도는 그대로다. Live 레코드가 카운트
   중 갱신되고, `Cmd:Abort`가 실행 중 명령을 멈춘다.
3. mar face — pck 인코딩이 `get_pck` 왕복으로 일치하고, mar345-ioc가 sim에서
   프레임을 받는다. sim의 `mar345expose`가 mar345-ioc를 거쳐 파일을 남긴다.
4. bl5a-qs — 정렬 plan이 sim의 참 오프셋을 허용오차 안에서 찾는다. fly
   스캔의 이벤트가 SPEC의 SCAN_D 앞 NPTS행과 같고, 중단하면 SPEC 명령이
   abort되고 속도가 복구된다. 피크 헬퍼가 같은 배열에서 `splot_DataStatistics.py`와 같은 값을
   낸다. 네 plan이 sim에서 끝까지 돌고, sim이 받은 `spec_cmd` 순서가 원본
   매크로에서 같은 명령만 골라낸 순서와 같다(`mar345expose`는 위 노출
   조각의 SPEC 명령들로 펼친 순서). 노출 중 중단하면 카운트가 멈추고 셔터가
   닫힌다.
5. integrator — sim 프레임의 피크 q가 sim d-spacing과 bin 폭 안에서 일치한다.
6. GUI.
7. temp-ioc — sim LakeShore face 상대로 설정점·램프·읽기, 그다음 In-situ plan.
   그 전까지 In-situ plan은 SPEC 온도 매크로(ser 9)로 돈다.
8. 빔라인: 실제 SPEC에 브리지를 읽기 전용으로 먼저 붙이고 → 안전한 모터
   소이동 → 카운트 → 정렬 스캔(같은 범위의 SPEC 스캔과 피크 값 비교) → mar
   노출 순으로 진행한다. 온도는 ser 9 매크로를
   `BL5A:TC:*`로 바꾼 뒤 IOC에 포트를 넘긴다.
