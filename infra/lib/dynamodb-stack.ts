import type { StackProps } from 'aws-cdk-lib';
import { RemovalPolicy, Stack } from 'aws-cdk-lib';
import { AttributeType, BillingMode, ProjectionType, Table } from 'aws-cdk-lib/aws-dynamodb';
import type { Construct } from 'constructs';

export class DynamoDbStack extends Stack {
	public readonly engineersApiKeyIndexName = 'ApiKeyIndex';
	public readonly engineersTable: Table;
	public readonly messagesTable: Table;
	public readonly rateLimitTable: Table;

	public constructor(scope: Construct, id: string, props?: StackProps) {
		super(scope, id, props);

		this.engineersTable = new Table(this, 'EngineersTable', {
			partitionKey: {
				name: 'user_id',
				type: AttributeType.STRING,
			},
			billingMode: BillingMode.PAY_PER_REQUEST,
			removalPolicy: RemovalPolicy.RETAIN,
		});

		this.engineersTable.addGlobalSecondaryIndex({
			indexName: this.engineersApiKeyIndexName,
			partitionKey: {
				name: 'api_key_hash',
				type: AttributeType.STRING,
			},
			projectionType: ProjectionType.ALL,
		});

		this.messagesTable = new Table(this, 'MessagesTable', {
			partitionKey: {
				name: 'user_id',
				type: AttributeType.STRING,
			},
			sortKey: {
				name: 'message_id',
				type: AttributeType.STRING,
			},
			billingMode: BillingMode.PAY_PER_REQUEST,
			removalPolicy: RemovalPolicy.RETAIN,
		});

		this.rateLimitTable = new Table(this, 'RateLimitTable', {
			partitionKey: {
				name: 'user_id',
				type: AttributeType.STRING,
			},
			sortKey: {
				name: 'request_ts',
				type: AttributeType.NUMBER,
			},
			timeToLiveAttribute: 'ttl',
			billingMode: BillingMode.PAY_PER_REQUEST,
			removalPolicy: RemovalPolicy.RETAIN,
		});
	}
}
