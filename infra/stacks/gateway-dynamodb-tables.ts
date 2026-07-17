import type { RemovalPolicy } from 'aws-cdk-lib';
import { AttributeType, BillingMode, ProjectionType, Table } from 'aws-cdk-lib/aws-dynamodb';
import { Construct } from 'constructs';

type GatewayDynamoDbTablesProps = {
	removalPolicy: RemovalPolicy;
};

export class GatewayDynamoDbTables extends Construct {
	public readonly engineersApiKeyIndexName = 'ApiKeyIndex';
	public readonly engineersTable: Table;
	public readonly messagesTable: Table;
	public readonly rateLimitTable: Table;
	public readonly tokenUsageTable: Table;

	public constructor(scope: Construct, id: string, props: GatewayDynamoDbTablesProps) {
		super(scope, id);

		this.engineersTable = new Table(this, 'EngineersTable', {
			partitionKey: {
				name: 'user_id',
				type: AttributeType.STRING,
			},
			billingMode: BillingMode.PAY_PER_REQUEST,
			removalPolicy: props.removalPolicy,
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
			removalPolicy: props.removalPolicy,
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
			removalPolicy: props.removalPolicy,
		});

		this.tokenUsageTable = new Table(this, 'TokenUsageTable', {
			partitionKey: {
				name: 'user_id',
				type: AttributeType.STRING,
			},
			sortKey: {
				name: 'usage_window',
				type: AttributeType.STRING,
			},
			timeToLiveAttribute: 'ttl',
			billingMode: BillingMode.PAY_PER_REQUEST,
			removalPolicy: props.removalPolicy,
		});
	}
}
