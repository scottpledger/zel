from hconf import VBZ,PORT
import http.server,json,sys,os,base64
os.chdir(VBZ+'/www')
class H(http.server.SimpleHTTPRequestHandler):
    def log_message(self,*a): pass
    def do_GET(self):
        p=self.path
        if p.startswith('/echo'):
            h={k.lower():v for k,v in self.headers.items() if k.lower() in('authorization','x-custom','accept','user-agent','cookie')}
            h.pop('user-agent',None)
            b=json.dumps(h,sort_keys=True).encode(); self.send_response(200); self.send_header('Content-Length',str(len(b))); self.end_headers(); self.wfile.write(b); return
        if p.startswith('/auth'):
            a=self.headers.get('Authorization','')
            if a!='Basic '+base64.b64encode(b'user:pass').decode():
                self.send_response(401); self.send_header('WWW-Authenticate','Basic realm="x"'); self.send_header('Content-Length','0'); self.end_headers(); return
            b=b'secret\n'; self.send_response(200); self.send_header('Content-Length',str(len(b))); self.end_headers(); self.wfile.write(b); return
        if p.startswith('/redir'):
            self.send_response(302); self.send_header('Location','/hello.txt'); self.send_header('Content-Length','0'); self.end_headers(); return
        if p.startswith('/loop'):
            self.send_response(302); self.send_header('Location','/loop'); self.send_header('Content-Length','0'); self.end_headers(); return
        if p.startswith('/err500'):
            self.send_response(500); self.send_header('Content-Length','0'); self.end_headers(); return
        if p.startswith('/noname'):
            self.path='/small.tar.gz'
        return super().do_GET()
http.server.ThreadingHTTPServer(('127.0.0.1',int(PORT)),H).serve_forever()
