select
    c.region,
    a.currency,
    sum(a.avg_order_value) as summed_aov,
    avg(a.avg_order_value) as mean_aov,
    sum(a.revenue) / sum(a.orders) as weighted_aov
from {{ ref('customer_aov') }} a
join {{ source('metrics', 'customers') }} c on a.customer_id = c.customer_id
group by c.region, a.currency
