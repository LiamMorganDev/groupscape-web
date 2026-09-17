const express = require('express');
const winston = require('winston');
const expressWinston = require('express-winston');
const path = require('path');
const compression = require('compression');
const axios = require('axios');
const http = require('http');
const https = require('https');
const { URL } = require('url');
const app = express();
const port = 4000;

const args = process.argv.map((arg) => arg.trim());
function getArgValue(arg) {
  const i = args.indexOf(arg);
  if (i === -1) return;
  return args[i + 1];
}

const backend = getArgValue('--backend') === undefined ? process.env.HOST_URL : getArgValue('--backend');

app.use(expressWinston.logger({
  transports: [
    new winston.transports.Console()
  ],
  format: winston.format.combine(
    winston.format.colorize(),
    winston.format.simple()
  ),
  meta: false,
  msg: "HTTP {{req.method}} {{req.url}} {{res.statusCode}}",
  expressFormat: false,
  colorize: true,
  metaField: null
}));
app.use(compression());
app.use(express.static('public'));
app.use(express.static('.'));

if (backend) {
  console.log(`Backend for api calls: ${backend}`);
  app.use(express.json());
  app.use('/api*', (req, res, next) => {
    const forwardUrl = backend + req.originalUrl;
    console.log(`Calling backend ${forwardUrl}`);
    const headers = Object.assign({}, req.headers);
    delete headers.host;
    delete headers.referer;
    axios({
      method: req.method,
      url: forwardUrl,
      responseType: 'stream',
      headers,
      data: req.body
    }).then((response) => {
      res.status(response.status);
      res.set(response.headers);
      response.data.pipe(res);
    }).catch((error) => {
      if (error.response) {
        res.status(error.response.status);
        res.set(error.response.headers);
        error.response.data.pipe(res);
      } else if (error.request) {
        res.status(418).end();
      } else {
        console.error('Error', error.message);
        res.status(418).end();
      }
    });
  });
} else {
  console.log("No backend supplied for api calls, not going to handle api requests");
}

app.get('*', function (request, response) {
  if (request.path.includes('/map') && request.path.includes('.png')) {
    response.sendStatus(404);
  } else {
    response.sendFile(path.resolve('public', 'index.html'));
  }
});

const server = app.listen(port, () => {
  console.log(`Listening on http://localhost:${port}`);
});

// The `/api*` handler above only forwards plain HTTP request/response pairs via axios - a
// WebSocket handshake is an HTTP `Upgrade` request that never reaches Express's middleware stack
// at all (Node dispatches it through the server's own `upgrade` event instead), so without this
// the chat drawer's `wss://.../api/group/{name}/ws` connection silently never reaches the backend
// in production - it looks like chat "isn't live" because every update only ever arrives via the
// next page load's REST backfill. Same raw-pipe pattern the RuneLite plugin's own websockets use
// connecting directly to the backend - here it's just relayed through this frontend proxy first.
if (backend) {
  server.on('upgrade', (req, socket, head) => {
    if (!req.url.startsWith('/api')) {
      socket.destroy();
      return;
    }

    const backendUrl = new URL(backend);
    const client = backendUrl.protocol === 'https:' ? https : http;

    const headers = Object.assign({}, req.headers);
    delete headers.host;

    const proxyReq = client.request({
      hostname: backendUrl.hostname,
      port: backendUrl.port || (backendUrl.protocol === 'https:' ? 443 : 80),
      path: req.url,
      method: req.method,
      headers,
    });

    proxyReq.on('upgrade', (proxyRes, proxySocket, proxyHead) => {
      const statusLine = `HTTP/1.1 ${proxyRes.statusCode} ${proxyRes.statusMessage}`;
      const responseHeaders = Object.entries(proxyRes.headers)
        .map(([key, value]) => `${key}: ${value}`)
        .join('\r\n');
      socket.write(`${statusLine}\r\n${responseHeaders}\r\n\r\n`);
      if (proxyHead && proxyHead.length) proxySocket.unshift(proxyHead);
      if (head && head.length) socket.unshift(head);
      proxySocket.pipe(socket);
      socket.pipe(proxySocket);
      proxySocket.on('error', () => socket.destroy());
      socket.on('error', () => proxySocket.destroy());
    });

    proxyReq.on('error', (error) => {
      console.error('WebSocket proxy error', error.message);
      socket.destroy();
    });

    proxyReq.end();
  });
}
