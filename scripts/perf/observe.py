"""One-second process/cgroup/SDK samples; query snapshots are taken by sweep.sh."""
import json, os, pathlib, sys, time, urllib.request
out=open(sys.argv[1], 'w', buffering=1)
pids=list(map(int, sys.argv[2:]))
while True:
    row={'time':time.time(),'processes':{}}
    for pid in pids:
        try:
            base=pathlib.Path(f'/proc/{pid}')
            stat=(base/'stat').read_text().split()
            row['processes'][str(pid)]={'cpu_ticks':int(stat[13])+int(stat[14]),'rss_bytes':int(stat[23])*os.sysconf('SC_PAGE_SIZE'),'threads':int(stat[19]),'io':(base/'io').read_text()}
        except (OSError,IndexError): pass
    for name,url in [('temporal','http://127.0.0.1:19092/metrics'),('simulator','http://127.0.0.1:19090/metrics')]:
        try:
            row[name]=urllib.request.urlopen(url,timeout=.5).read().decode()
        except Exception as e: row[name+'_error']=str(e)
    out.write(json.dumps(row)+'\n');time.sleep(1)
