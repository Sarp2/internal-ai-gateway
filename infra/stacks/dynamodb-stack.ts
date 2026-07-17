import type { StackProps } from 'aws-cdk-lib';
import { RemovalPolicy, Stack } from 'aws-cdk-lib';
import type { Table } from 'aws-cdk-lib/aws-dynamodb';
import type { Construct } from 'constructs';
import { GatewayDynamoDbTables } from './gateway-dynamodb-tables.ts';

export class DynamoDbStack extends Stack {
	public readonly engineersApiKeyIndexName: string;
	public readonly engineersTable: Table;
	public readonly messagesTable: Table;
	public readonly rateLimitTable: Table;
	public readonly tokenUsageTable: Table;

	public constructor(scope: Construct, id: string, props?: StackProps) {
		super(scope, id, props);

		const tables = new GatewayDynamoDbTables(this, 'Tables', {
			removalPolicy: RemovalPolicy.RETAIN,
		});

		this.engineersApiKeyIndexName = tables.engineersApiKeyIndexName;
		this.engineersTable = tables.engineersTable;
		this.messagesTable = tables.messagesTable;
		this.rateLimitTable = tables.rateLimitTable;
		this.tokenUsageTable = tables.tokenUsageTable;
	}
}
