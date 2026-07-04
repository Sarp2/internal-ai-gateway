import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Duration, Stack, type StackProps } from 'aws-cdk-lib';
import { Metric, Unit } from 'aws-cdk-lib/aws-cloudwatch';
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
} from 'aws-cdk-lib/aws-elasticloadbalancingv2';
import { PolicyStatement } from 'aws-cdk-lib/aws-iam';
import { LogGroup, RetentionDays } from 'aws-cdk-lib/aws-logs';
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
			metric: new Metric({
				namespace: activeStreamMetricNamespace,
				metricName: activeStreamsMetricName,
				dimensionsMap: {
					ServiceName: proxyServiceName,
				},
				statistic: 'Average',
				period: Duration.seconds(60),
				unit: Unit.COUNT,
			}),
			targetValue: proxyActiveStreamsScaleTarget,
			scaleInCooldown: Duration.seconds(180),
			scaleOutCooldown: Duration.seconds(30),
		});
	}
}
