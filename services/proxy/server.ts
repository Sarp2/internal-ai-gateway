import { createServer } from 'node:http';

const defaultPort = 8080;

export function createProxyServer() {
	return createServer((request, response) => {
		if (request.method === 'GET' && request.url === '/health') {
			response.writeHead(200, {
				'content-type': 'application/json',
			});
			response.end(JSON.stringify({ status: 'ok' }));
			return;
		}

		response.writeHead(404, {
			'content-type': 'application/json',
		});
		response.end(JSON.stringify({ message: 'Route not found.' }));
	});
}

if (import.meta.url === `file://${process.argv[1]}`) {
	const port = Number(process.env.PORT ?? defaultPort);
	const server = createProxyServer();

	server.listen(port, () => {
		console.log(`Proxy service listening on port ${port}`);
	});
}
