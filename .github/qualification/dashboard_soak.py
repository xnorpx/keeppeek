"""Run two loopback recorders against the same camera through identical RTSP relays."""
import asyncio
from collections import defaultdict
import fcntl
import json
import os
from pathlib import Path
import pty
import signal
import socket
import sqlite3
import statistics
import struct
import subprocess
import sys
import tempfile
import termios
import time
import tomllib
from urllib.parse import urlsplit, urlunsplit

BIN = Path(sys.argv[1]).resolve()
CAM = Path(sys.argv[2]).resolve()
MEDIA = Path(sys.argv[3]).resolve()
DURATION = int(sys.argv[4]) if len(sys.argv) > 4 else 1800
INTERVAL = min(10, DURATION // 18)
MIB = 1024 * 1024

class Relay:
    def __init__(self, port):
        self.port = port
        self.clients = set()
        self.accepted = 0
        self.open = True
        self.epoch = 0
        self.media_epoch = -1
        self.attempts = []

    async def accept(self, reader, writer):
        try:
            epoch = self.epoch
            evidence = dict(epoch=epoch, playing=False, client_bytes=0, camera_bytes=0,
                            camera_bytes_after_play=0, error=None)
            self.attempts.append(evidence)
            if not self.open:
                evidence["error"] = "fixture disconnected"
                return
            upstream, out = await asyncio.open_connection('127.0.0.1', self.port)
            if not self.open or epoch != self.epoch:
                out.close()
                evidence["error"] = "fixture disconnected during attach"
                return
            self.accepted += 1
            self.clients.add(writer)
            self.clients.add(out)
            async def copy(src, dst, from_camera):
                tail = b""
                while data := await src.read(65536):
                    if from_camera:
                        evidence["camera_bytes"] += len(data)
                        if evidence["playing"]:
                            evidence["camera_bytes_after_play"] += len(data)
                            if epoch == self.epoch and evidence["camera_bytes_after_play"] >= 8192:
                                self.media_epoch = epoch
                    else:
                        evidence["client_bytes"] += len(data)
                        evidence["playing"] |= b"PLAY " in tail + data
                        tail = data[-4:]
                    dst.write(data)
                    await dst.drain()
            a = asyncio.create_task(copy(reader, out, False))
            b = asyncio.create_task(copy(upstream, writer, True))
            try:
                await asyncio.wait([a,b], return_when=asyncio.FIRST_COMPLETED)
            finally:
                a.cancel()
                b.cancel()
                await asyncio.gather(a,b,return_exceptions=True)
                out.close()
                self.clients.discard(out)
        except (ConnectionError, OSError) as error:
            evidence["error"] = type(error).__name__
        finally:
            writer.close()
            self.clients.discard(writer)

    def disconnect(self):
        self.open = False
        self.epoch += 1
        for conn in list(self.clients):
            conn.transport.abort()

def port():
    with socket.socket() as s:
        s.bind(('127.0.0.1',0))
        return s.getsockname()[1]

def rss(pid):
    for line in Path(f'/proc/{pid}/status').read_text().splitlines():
        if line.startswith('VmRSS:'):
            return int(line.split()[1])*1024
    raise RuntimeError(f'missing process RSS for {pid}')

def memory_breakdown(pid):
    # PSS and private/anonymous breakdown help distinguish process allocations
    # from ordinary sharing and resident file mappings.
    source = Path(f'/proc/{pid}/smaps_rollup').read_text()
    fields = {}
    for line in source.splitlines():
        field, _, value = line.partition(':')
        if field in ('Rss', 'Pss', 'Anonymous', 'Private_Clean', 'Private_Dirty'):
            fields[field] = int(value.split()[0]) * 1024
    assert len(fields) == 5, fields.keys()
    return fields


def window(fd, cols, rows):
    fcntl.ioctl(fd,termios.TIOCSWINSZ,struct.pack('HHHH',rows,cols,0,0))

def continuity(directory):
    streams = defaultdict(list)
    catalogs = list((directory / 'storage' / 'metadata').glob('catalog-*.db'))
    assert len(catalogs) == 1, f'expected one managed catalog, found {catalogs}'
    catalog, = catalogs
    with sqlite3.connect(catalog.as_uri() + '?mode=ro', uri=True) as db:
        recordings = db.execute(
            'SELECT logical_stream_id, started_at_ms, path FROM recording_files ORDER BY started_at_ms, id'
        ).fetchall()
    assert recordings, 'no cataloged recordings'
    for stream, start_ms, path in recordings:
        file = Path(path)
        if not file.is_absolute():
            file = directory / file
        assert file.is_file() and file.resolve().is_relative_to(directory), file
        out = subprocess.check_output(['ffprobe','-v','error','-select_streams','v:0','-show_entries','frame=best_effort_timestamp_time','-of','json',file])
        pts = sorted(float(frame['best_effort_timestamp_time']) for frame in json.loads(out)['frames'] if 'best_effort_timestamp_time' in frame)
        assert pts, f'no decoded video frames in {file}'
        streams[stream].extend(start_ms / 1000 + t - pts[0] for t in pts)
    return {stream:sorted(times) for stream,times in streams.items()}

async def camera_configuration(folder, processes, handles):
    camera_log = (folder / "camera.log").open("wb")
    handles.append(camera_log)
    camera = await asyncio.create_subprocess_exec(
        CAM, "rtsp", "--main", MEDIA, "--sub", MEDIA, "--name", "soak",
        "--start-at-seconds", "0", stdout=asyncio.subprocess.PIPE, stderr=camera_log,
    )
    processes.append(camera)
    raw = b""
    while b"transport = " not in raw:
        line = await asyncio.wait_for(camera.stdout.readline(), 20)
        assert line, "camera exited before configuration"
        raw += line
    return raw.decode()


async def main(work):
    procs=[]
    handles=[]
    servers=[]
    relays={}
    recorders={}
    samples=[]
    memory=[]
    forced=[]
    master=slave=None
    markers=set()
    carry=b''
    try:
        loop=asyncio.get_running_loop()
        for mode in ['off','on']:
            folder=work/mode
            folder.mkdir()
            # The fixture's metadata relay owns one RTSP connection per profile.
            settings=await camera_configuration(folder, procs, handles)
            conf=tomllib.loads(settings)['test-camera']['soak']
            for source in ['main','sub']:
                url=conf[source+'_rtsp_url']
                endpoint=urlsplit(url)
                relay=Relay(endpoint.port)
                server=await asyncio.start_server(relay.accept,'127.0.0.1',0)
                servers.append(server)
                relays[mode,source]=relay
                netloc='127.0.0.1:'+str(server.sockets[0].getsockname()[1])
                if '@' in endpoint.netloc: netloc=endpoint.netloc.split('@',1)[0]+'@'+netloc
                proxy=urlunsplit(endpoint._replace(netloc=netloc))
                settings=settings.replace(f'{source}_rtsp_url = "{url}"',f'{source}_rtsp_url = "{proxy}"')
            rec=folder/'recordings'
            tmp=folder/'medium'
            cfg=(f'host = "127.0.0.1"\nport = {port()}\n[storage]\nshort_term_secs = 5\n'
                 f'medium_term_secs = 20\nflush_interval_secs = 2\nlong_term_max_gb = 0\n'
                 'minimum_free_gb = 1\ncritical_free_gb = 0\n'
                 f'long_term_path = "{rec}"\nmedium_term_path = "{tmp}"\n'
                 +settings+'\nrecording_mode = "both"\n')
            (folder/'config.toml').write_text(cfg)
            env=dict(os.environ,HOME=str(folder),XDG_CONFIG_HOME=str(folder),TERM='xterm-256color',NO_COLOR='1')
            if mode=='on':
                master,slave=pty.openpty()
                window(master,120,40)
                def terminal_owner():
                    os.setsid()
                    fcntl.ioctl(slave,termios.TIOCSCTTY,0)
                options=dict(stdin=slave,stdout=slave,stderr=slave,preexec_fn=terminal_owner)
            else:
                log=(folder/'server.log').open('wb')
                handles.append(log)
                options=dict(stdin=subprocess.DEVNULL,stdout=log,stderr=log,start_new_session=True)
            proc=await asyncio.create_subprocess_exec(BIN,'--config','config.toml','--tui','always' if mode=='on' else 'never',cwd=folder,env=env,**options)
            recorders[mode]=proc
            procs.append(proc)
        os.close(slave)
        slave=None
        def drain():
            nonlocal carry
            try: data=os.read(master,65536)
            except OSError:
                loop.remove_reader(master)
                return
            raw=carry+data
            for label in (b'Uptime',b'Enlarge',b'\x1b[?1049l'):
                if label in raw:markers.add(label)
            carry=raw[-4000:]
        loop.add_reader(master,drain)
        for _ in range(90):
            if b'Uptime' in markers and all(r.media_epoch == 0 for r in relays.values()):break
            assert all(p.returncode is None for p in procs), (
                'fixture stopped during startup', [p.returncode for p in procs],
                [(work/mode/'camera.log').read_text(errors='replace')[-2000:] for mode in recorders],
                (work/'off'/'server.log').read_text(errors='replace')[-4000:],
                carry.decode(errors='backslashreplace'),
            )
            await asyncio.sleep(1)
        assert b'Uptime' in markers and all(r.media_epoch == 0 for r in relays.values()), (markers,{str(k):r.accepted for k,r in relays.items()})
        start=time.monotonic()
        while (elapsed:=time.monotonic()-start)<DURATION:
            assert all(p.returncode is None for p in procs), 'fixture stopped during soak'
            iteration=len(samples)
            samples.append([round(elapsed,3),rss(recorders['off'].pid),rss(recorders['on'].pid)])
            if iteration % max(1, 60 // INTERVAL) == 0:
                memory.append({
                    'elapsed': round(elapsed, 3),
                    'recorders': {mode: memory_breakdown(proc.pid) for mode, proc in recorders.items()},
                })
            window(master,34 if iteration%3==0 else 120,7 if iteration%3==0 else 40)
            os.write(master,[b'\x1b[B',b'\x1b[6~',b'\x1b[5~'][iteration%3])
            if iteration in (DURATION // INTERVAL * 4 // 15, DURATION // INTERVAL * 3 // 5):
                forced.append([time.time(),None])
                for r in relays.values():r.disconnect()
            if iteration in (DURATION // INTERVAL * 4 // 15 + 1, DURATION // INTERVAL * 3 // 5 + 1):
                for r in relays.values():r.open=True
            if forced and forced[-1][1] is None and all(r.media_epoch == r.epoch for r in relays.values()):
                forced[-1][1]=time.time()
            if iteration%12==0:
                print('soak', samples[-1], 'sessions',
                      {str(k): (r.accepted, r.media_epoch, r.epoch) for k,r in relays.items()},
                      flush=True)
            await asyncio.sleep(max(0,min(INTERVAL*(iteration+1),DURATION)-(time.monotonic()-start)))
        assert len(forced)==2 and all(end for _,end in forced),forced
        assert b'Enlarge' in markers
        os.write(master,b'q')
        for _ in range(50):
            if b'\x1b[?1049l' in markers:break
            await asyncio.sleep(.1)
        assert b'\x1b[?1049l' in markers and recorders['on'].returncode is None
        def incoming_camera_bytes(relay):
            return sum(attempt['camera_bytes_after_play'] for attempt in relay.attempts)

        before_detach_cooldown = {key: incoming_camera_bytes(relay) for key, relay in relays.items()}
        after_detach = []
        cooldown = min(90, DURATION // 4)
        detach_time = time.monotonic()
        for second in range(0, cooldown + 1, 5):
            await asyncio.sleep(max(0, detach_time + second - time.monotonic()))
            assert all(proc.returncode is None for proc in recorders.values())
            after_detach.append({
                'seconds_after_detach': round(time.monotonic() - detach_time, 3),
                'recorders': {mode: memory_breakdown(proc.pid) for mode, proc in recorders.items()},
            })
        assert all(incoming_camera_bytes(relay) > before_detach_cooldown[key]
                   for key, relay in relays.items()), 'camera input stopped after dashboard detach'
        for proc in recorders.values():proc.send_signal(signal.SIGINT)
        for proc in recorders.values(): assert await asyncio.wait_for(proc.wait(),30)==0
        first=[on-off for t,off,on in samples if DURATION/15<=t<DURATION*7/30]
        last=[on-off for t,off,on in samples if t>=DURATION*5/6]
        report={'seconds':detach_time-start,'samples':samples,'forced_disconnects':forced,
            'early_median_rss_delta':statistics.median(first),'late_median_rss_delta':statistics.median(last),
            'memory_breakdown':memory,'after_detach':after_detach,'streams':{}}
        for mode in recorders:
            streams=continuity(work/mode)
            assert len(streams)==2,(mode,list(streams))
            for stream,pts in streams.items():
                assert len(pts)>DURATION*(20 if DURATION>=1800 else 10),(stream,len(pts))
                gaps=[(b-a,a,b) for a,b in zip(pts,pts[1:]) if b-a>0.085]
                report['streams'][mode+'/'+stream]={'frames':len(pts),'first':pts[0],'last':pts[-1],
                    'gaps_outside_reconnect':[(round(g,3),a) for g,a,b in gaps if not any(a<=r+5 and l-5<=b for l,r in forced)],
                    'largest_gap_s':max((g[0] for g in gaps),default=0)}
        output=Path(os.environ['RUNNER_TEMP'])/'dashboard-soak.json'
        output.write_text(json.dumps(report,indent=2))
        for key, value in report["streams"].items():
            gaps=value["gaps_outside_reconnect"]
            print(key, "decoded frames", value["frames"], "gaps over 85ms outside forced reconnects", len(gaps), "first ten", gaps[:10], flush=True)
        print("RSS delta median early / late", report["early_median_rss_delta"], report["late_median_rss_delta"], flush=True)
    except Exception:
        evidence = {f"{mode}/{source}": relay.attempts for (mode, source), relay in relays.items()}
        for key, attempts in evidence.items():
            print("RTSP relay", key, json.dumps(attempts[-12:]), flush=True)
        for log in (work / "off" / "camera.log", work / "on" / "camera.log", work / "off" / "server.log"):
            if log.is_file():
                with log.open("rb") as handle:
                    handle.seek(max(0, log.stat().st_size - 6000))
                    text = handle.read().decode(errors="replace")
                print(log.relative_to(work), text.replace("test:test@", "[redacted]@"), flush=True)
        print("foreground terminal tail", carry.decode(errors="backslashreplace")[-1800:], flush=True)
        (Path(os.environ["RUNNER_TEMP"]) / "dashboard-soak.json").write_text(
            json.dumps({"complete": False, "forced_disconnects": forced, "relays": evidence}, indent=2)
        )
        raise
    finally:
        for p in reversed(procs):
            if p.returncode is None:
                p.terminate()
                try: await asyncio.wait_for(p.wait(),5)
                except asyncio.TimeoutError:
                    p.kill()
                    await p.wait()
        for server in servers:
            server.close()
            await server.wait_closed()
        if master is not None:
            asyncio.get_running_loop().remove_reader(master)
            os.close(master)
        if slave is not None:os.close(slave)
        for item in handles:item.close()

if __name__=='__main__':
    with tempfile.TemporaryDirectory(prefix='keeppeek-soak-') as tmp:
        asyncio.run(main(Path(tmp)))
