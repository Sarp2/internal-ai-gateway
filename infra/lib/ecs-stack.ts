import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { CfnOutput, Duration, RemovalPolicy, Stack, type StackProps } from 'aws-cdk-lib';
import {
	Alarm,
	CfnDashboard,
	ComparisonOperator,
	Metric,
	TreatMissingData,
	Unit,
} from 'aws-cdk-lib/aws-cloudwatch';
import type { IVpc, Vpc } from 'aws-cdk-lib/aws-ec2';
import { Peer, Port, SecurityGroup, SubnetType } from 'aws-cdk-lib/aws-ec2';
import {
	Cluster,
	ContainerImage,
	ContainerInsights,
	FargateService,
	FargateTaskDefinition,
	type ICluster,
	LogDrivers,
} from 'aws-cdk-lib/aws-ecs';
import {
	ApplicationLoadBalancer,
	ApplicationProtocol,
	type ApplicationTargetGroup,
	HttpCodeTarget,
} from 'aws-cdk-lib/aws-elasticloadbalancingv2';
import { PolicyStatement, ServicePrincipal } from 'aws-cdk-lib/aws-iam';
import { LogGroup, RetentionDays } from 'aws-cdk-lib/aws-logs';
import { BlockPublicAccess, Bucket, BucketEncryption } from 'aws-cdk-lib/aws-s3';
import type { Construct } from 'constructs';

const currentDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = join(currentDirectory, '..', '..');
const proxyContainerPort = 8080;
const proxyDesiredTaskCount = 3;
const proxyMaxTaskCount = 30;
const proxyScaleTargetPercent = 60;
const proxyRequestsPerTarget = 1_000;
const proxyActiveStreamsScaleTarget = 150;
const proxyMaxActiveStreams = 200;
const activeStreamMetricNamespace = 'InternalAiGateway/Proxy';
const activeStreamsMetricName = 'ActiveStreams';
const proxyServiceName = 'internal-ai-gateway-proxy';
const proxyAccessLogPrefix = 'alb';

type EcsStackProps = StackProps & {
	vpc: Vpc;
};

export class EcsStack extends Stack {
	public readonly cluster: Cluster;
	public readonly proxyLoadBalancer: ApplicationLoadBalancer;
	public readonly proxyLoadBalancerSecurityGroup: SecurityGroup;
	public readonly proxyTargetGroup: ApplicationTargetGroup;
	public readonly proxyService: FargateService;
	public readonly proxyServiceSecurityGroup: SecurityGroup;
	public readonly proxyTaskDefinition: FargateTaskDefinition;
	public readonly proxyLogGroup: LogGroup;
	public readonly proxyAccessLogBucket: Bucket;

	public constructor(scope: Construct, id: string, props: EcsStackProps) {
		super(scope, id, props);

		this.cluster = new Cluster(this, 'ProxyCluster', {
			// CDK's concrete Vpc is compatible with IVpc, but exactOptionalPropertyTypes
			// rejects one optional property in the upstream type relationship.
			vpc: props.vpc as IVpc,
			clusterName: 'internal-ai-gateway-proxy',
			enableFargateCapacityProviders: true,
			containerInsightsV2: ContainerInsights.ENHANCED,
		});

		this.proxyLogGroup = new LogGroup(this, 'ProxyLogGroup', {
			logGroupName: '/internal-ai-gateway/proxy',
			retention: RetentionDays.ONE_MONTH,
		});

		this.proxyAccessLogBucket = new Bucket(this, 'ProxyAccessLogBucket', {
			blockPublicAccess: BlockPublicAccess.BLOCK_ALL,
			encryption: BucketEncryption.S3_MANAGED,
			enforceSSL: true,
			removalPolicy: RemovalPolicy.RETAIN,
		});

		this.proxyAccessLogBucket.addToResourcePolicy(
			new PolicyStatement({
				actions: ['s3:PutObject'],
				principals: [new ServicePrincipal('logdelivery.elasticloadbalancing.amazonaws.com')],
				resources: [
					this.proxyAccessLogBucket.arnForObjects(
						`${proxyAccessLogPrefix}/AWSLogs/${this.account}/*`,
					),
				],
				conditions: {
					StringEquals: {
						's3:x-amz-acl': 'bucket-owner-full-control',
					},
				},
			}),
		);

		this.proxyAccessLogBucket.addToResourcePolicy(
			new PolicyStatement({
				actions: ['s3:GetBucketAcl'],
				principals: [new ServicePrincipal('logdelivery.elasticloadbalancing.amazonaws.com')],
				resources: [this.proxyAccessLogBucket.bucketArn],
			}),
		);

		this.proxyTaskDefinition = new FargateTaskDefinition(this, 'ProxyTaskDefinition', {
			family: proxyServiceName,
			cpu: 512,
			memoryLimitMiB: 1024,
		});

		this.proxyTaskDefinition.addToTaskRolePolicy(
			new PolicyStatement({
				actions: ['cloudwatch:PutMetricData'],
				resources: ['*'],
				conditions: {
					StringEquals: {
						'cloudwatch:namespace': activeStreamMetricNamespace,
					},
				},
			}),
		);

		this.proxyTaskDefinition.addContainer('ProxyContainer', {
			containerName: 'proxy',
			image: ContainerImage.fromAsset(repositoryRoot, {
				exclude: [
					'.git',
					'cdk.out',
					'dist',
					'infra/cdk.out',
					'node_modules',
					'services/proxy/target',
					'target',
				],
				file: 'services/proxy/Dockerfile',
			}),
			essential: true,
			environment: {
				ACTIVE_STREAM_METRIC_INTERVAL_SECONDS: '15',
				MAX_ACTIVE_STREAMS: String(proxyMaxActiveStreams),
				PORT: String(proxyContainerPort),
				RUST_LOG: 'info',
			},
			logging: LogDrivers.awsLogs({
				logGroup: this.proxyLogGroup,
				streamPrefix: 'proxy',
			}),
			portMappings: [
				{
					containerPort: proxyContainerPort,
				},
			],
		});

		this.proxyServiceSecurityGroup = new SecurityGroup(this, 'ProxyServiceSecurityGroup', {
			vpc: props.vpc as IVpc,
			description: 'Controls network access for proxy ECS tasks.',
			allowAllOutbound: true,
		});

		this.proxyService = new FargateService(this, 'ProxyService', {
			cluster: this.cluster as ICluster,
			taskDefinition: this.proxyTaskDefinition,
			serviceName: proxyServiceName,
			desiredCount: proxyDesiredTaskCount,
			assignPublicIp: false,
			vpcSubnets: {
				subnetType: SubnetType.PRIVATE_WITH_EGRESS,
			},
			securityGroups: [this.proxyServiceSecurityGroup],
			minHealthyPercent: 100,
			maxHealthyPercent: 200,
			circuitBreaker: {
				rollback: true,
			},
			enableECSManagedTags: true,
		});

		this.proxyLoadBalancerSecurityGroup = new SecurityGroup(
			this,
			'ProxyLoadBalancerSecurityGroup',
			{
				vpc: props.vpc as IVpc,
				description: 'Allows public HTTP traffic to reach the proxy load balancer.',
				allowAllOutbound: true,
			},
		);

		this.proxyLoadBalancerSecurityGroup.addIngressRule(
			Peer.anyIpv4(),
			Port.tcp(80),
			'Allow public HTTP traffic.',
		);

		this.proxyServiceSecurityGroup.addIngressRule(
			this.proxyLoadBalancerSecurityGroup,
			Port.tcp(proxyContainerPort),
			'Allow proxy traffic from the load balancer.',
		);

		this.proxyLoadBalancer = new ApplicationLoadBalancer(this, 'ProxyLoadBalancer', {
			vpc: props.vpc as IVpc,
			internetFacing: true,
			idleTimeout: Duration.seconds(300),
			securityGroup: this.proxyLoadBalancerSecurityGroup,
			vpcSubnets: {
				subnetType: SubnetType.PUBLIC,
			},
		});

		this.proxyLoadBalancer.setAttribute('access_logs.s3.enabled', 'true');
		this.proxyLoadBalancer.setAttribute(
			'access_logs.s3.bucket',
			this.proxyAccessLogBucket.bucketName,
		);

		this.proxyLoadBalancer.setAttribute('access_logs.s3.prefix', proxyAccessLogPrefix);
		this.proxyLoadBalancer.setAttribute('idle_timeout.timeout_seconds', '300');

		const proxyListener = this.proxyLoadBalancer.addListener('ProxyHttpListener', {
			port: 80,
			protocol: ApplicationProtocol.HTTP,
			open: false,
		});

		this.proxyTargetGroup = proxyListener.addTargets('ProxyTargets', {
			port: proxyContainerPort,
			protocol: ApplicationProtocol.HTTP,
			deregistrationDelay: Duration.seconds(300),
			targets: [
				this.proxyService.loadBalancerTarget({
					containerName: 'proxy',
					containerPort: proxyContainerPort,
				}),
			],
			healthCheck: {
				path: '/health',
				healthyHttpCodes: '200',
				interval: Duration.seconds(10),
				timeout: Duration.seconds(5),
				healthyThresholdCount: 2,
				unhealthyThresholdCount: 3,
			},
		});

		const proxyScaling = this.proxyService.autoScaleTaskCount({
			minCapacity: proxyDesiredTaskCount,
			maxCapacity: proxyMaxTaskCount,
		});

		proxyScaling.scaleOnCpuUtilization('ProxyCpuScaling', {
			targetUtilizationPercent: proxyScaleTargetPercent,
		});

		proxyScaling.scaleOnMemoryUtilization('ProxyMemoryScaling', {
			targetUtilizationPercent: proxyScaleTargetPercent,
		});

		proxyScaling.scaleOnRequestCount('ProxyRequestCountScaling', {
			requestsPerTarget: proxyRequestsPerTarget,
			targetGroup: this.proxyTargetGroup,
			scaleInCooldown: Duration.seconds(120),
			scaleOutCooldown: Duration.seconds(60),
		});

		proxyScaling.scaleToTrackCustomMetric('ProxyActiveStreamScaling', {
			metric: this.activeStreamsMetric(),
			targetValue: proxyActiveStreamsScaleTarget,
			scaleInCooldown: Duration.seconds(180),
			scaleOutCooldown: Duration.seconds(30),
		});

		this.addObservability();

		new CfnOutput(this, 'ProxyLoadBalancerDnsName', {
			description: 'Public DNS name for the proxy Application Load Balancer.',
			value: this.proxyLoadBalancer.loadBalancerDnsName,
		});

		new CfnOutput(this, 'ProxyHealthUrl', {
			description: 'Health check URL for the proxy service.',
			value: `http://${this.proxyLoadBalancer.loadBalancerDnsName}/health`,
		});

		new CfnOutput(this, 'ProxyAccessLogBucketName', {
			description: 'S3 bucket that stores proxy ALB access logs.',
			value: this.proxyAccessLogBucket.bucketName,
		});
	}

	private addObservability(): void {
		const cpuMetric = this.proxyService.metricCpuUtilization({
			period: Duration.minutes(1),
		});

		const memoryMetric = this.proxyService.metricMemoryUtilization({
			period: Duration.minutes(1),
		});

		const activeStreamsMetric = this.activeStreamsMetric();

		const targetResponseTimeMetric = this.proxyTargetGroup.metrics.targetResponseTime({
			period: Duration.minutes(1),
		});

		const target5xxMetric = this.proxyTargetGroup.metrics.httpCodeTarget(
			HttpCodeTarget.TARGET_5XX_COUNT,
			{
				period: Duration.minutes(1),
			},
		);

		const unhealthyHostMetric = this.proxyTargetGroup.metrics.unhealthyHostCount({
			period: Duration.minutes(1),
		});

		new Alarm(this, 'ProxyHighCpuAlarm', {
			alarmDescription: 'Proxy ECS service CPU utilization is high.',
			comparisonOperator: ComparisonOperator.GREATER_THAN_OR_EQUAL_TO_THRESHOLD,
			evaluationPeriods: 3,
			metric: cpuMetric,
			threshold: 80,
			treatMissingData: TreatMissingData.NOT_BREACHING,
		});

		new Alarm(this, 'ProxyHighMemoryAlarm', {
			alarmDescription: 'Proxy ECS service memory utilization is high.',
			comparisonOperator: ComparisonOperator.GREATER_THAN_OR_EQUAL_TO_THRESHOLD,
			evaluationPeriods: 3,
			metric: memoryMetric,
			threshold: 80,
			treatMissingData: TreatMissingData.NOT_BREACHING,
		});

		new Alarm(this, 'ProxyHighActiveStreamsAlarm', {
			alarmDescription: 'Proxy active streams per task are close to the hard limit.',
			comparisonOperator: ComparisonOperator.GREATER_THAN_OR_EQUAL_TO_THRESHOLD,
			evaluationPeriods: 2,
			metric: activeStreamsMetric,
			threshold: 180,
			treatMissingData: TreatMissingData.NOT_BREACHING,
		});

		new Alarm(this, 'ProxyTarget5xxAlarm', {
			alarmDescription: 'Proxy targets are returning elevated 5xx responses.',
			comparisonOperator: ComparisonOperator.GREATER_THAN_OR_EQUAL_TO_THRESHOLD,
			evaluationPeriods: 2,
			metric: target5xxMetric,
			threshold: 10,
			treatMissingData: TreatMissingData.NOT_BREACHING,
		});

		new Alarm(this, 'ProxyUnhealthyTargetsAlarm', {
			alarmDescription: 'At least one proxy ALB target is unhealthy.',
			comparisonOperator: ComparisonOperator.GREATER_THAN_OR_EQUAL_TO_THRESHOLD,
			evaluationPeriods: 2,
			metric: unhealthyHostMetric,
			threshold: 1,
			treatMissingData: TreatMissingData.NOT_BREACHING,
		});

		new Alarm(this, 'ProxyHighResponseTimeAlarm', {
			alarmDescription: 'Proxy target response time is elevated.',
			comparisonOperator: ComparisonOperator.GREATER_THAN_OR_EQUAL_TO_THRESHOLD,
			evaluationPeriods: 3,
			metric: targetResponseTimeMetric,
			threshold: 5,
			treatMissingData: TreatMissingData.NOT_BREACHING,
		});

		new CfnDashboard(this, 'ProxyDashboard', {
			dashboardName: 'internal-ai-gateway-proxy',
			dashboardBody: Stack.of(this).toJsonString({
				widgets: [
					{
						type: 'metric',
						width: 12,
						height: 6,
						properties: {
							title: 'Proxy Load',
							region: Stack.of(this).region,
							metrics: [
								[
									'AWS/ApplicationELB',
									'RequestCount',
									'TargetGroup',
									this.proxyTargetGroup.targetGroupFullName,
									'LoadBalancer',
									this.proxyTargetGroup.firstLoadBalancerFullName,
									{ stat: 'Sum' },
								],
								[
									activeStreamMetricNamespace,
									activeStreamsMetricName,
									'ServiceName',
									proxyServiceName,
									{ stat: 'Average' },
								],
							],
						},
					},
					{
						type: 'metric',
						width: 12,
						height: 6,
						properties: {
							title: 'Proxy Resource Utilization',
							region: Stack.of(this).region,
							metrics: [
								[
									'AWS/ECS',
									'CPUUtilization',
									'ClusterName',
									this.cluster.clusterName,
									'ServiceName',
									proxyServiceName,
								],
								[
									'AWS/ECS',
									'MemoryUtilization',
									'ClusterName',
									this.cluster.clusterName,
									'ServiceName',
									proxyServiceName,
								],
							],
						},
					},
					{
						type: 'metric',
						width: 12,
						height: 6,
						properties: {
							title: 'Proxy Errors And Health',
							region: Stack.of(this).region,
							metrics: [
								[
									'AWS/ApplicationELB',
									HttpCodeTarget.TARGET_5XX_COUNT,
									'TargetGroup',
									this.proxyTargetGroup.targetGroupFullName,
									'LoadBalancer',
									this.proxyTargetGroup.firstLoadBalancerFullName,
									{ stat: 'Sum' },
								],
								[
									'AWS/ApplicationELB',
									'UnHealthyHostCount',
									'TargetGroup',
									this.proxyTargetGroup.targetGroupFullName,
									'LoadBalancer',
									this.proxyTargetGroup.firstLoadBalancerFullName,
									{ stat: 'Average' },
								],
							],
						},
					},
					{
						type: 'metric',
						width: 12,
						height: 6,
						properties: {
							title: 'Proxy Target Response Time',
							region: Stack.of(this).region,
							metrics: [
								[
									'AWS/ApplicationELB',
									'TargetResponseTime',
									'TargetGroup',
									this.proxyTargetGroup.targetGroupFullName,
									'LoadBalancer',
									this.proxyTargetGroup.firstLoadBalancerFullName,
									{ stat: 'Average' },
								],
							],
						},
					},
				],
			}),
		});
	}

	private activeStreamsMetric(): Metric {
		return new Metric({
			namespace: activeStreamMetricNamespace,
			metricName: activeStreamsMetricName,
			dimensionsMap: {
				ServiceName: proxyServiceName,
			},
			statistic: 'Average',
			period: Duration.seconds(60),
			unit: Unit.COUNT,
		});
	}
}
