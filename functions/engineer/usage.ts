export async function handler() {
	return {
		statusCode: 501,
		headers: {
			'content-type': 'application/json',
		},
		body: JSON.stringify({ message: 'Engineer usage is not implemented yet.' }),
	};
}
