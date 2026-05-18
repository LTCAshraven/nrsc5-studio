@echo off
REM Spike 0 Phase 3: rate-paced replay of captured I/Q through nrsc5.
REM Lives in scripts\ to keep the experiment self-documenting.
REM
REM Usage from project root:
REM     scripts\spike0-replay.bat
REM
REM Audio plays to default device via libao inside nrsc5.exe.
REM stderr from both processes is captured under target\.

target\iq_replay.exe target\spike0-iq.cu8 2> target\iq_replay-stderr.log | bin\nrsc5.exe -r - 0 2> target\spike0-replay.log
