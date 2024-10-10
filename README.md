# tick.rs

This is the code that runs [tick.rs](https://tick.rs/).

[tick.rs](https://tick.rs/) is a very simple service that allows you to increment and retrieve numbers. It's mostly useful for static websites where you don't want to make the leap into using a database. Instead you can use [tick.rs](https://tick.rs/). 

Data is persisted to an sqlite database. If you have a database on your server this will be of almost zero use to you, because it's just a column in a databse. But if you don't want to set up a database just to store some numbers, then tick.rs can be useful.

While nothing is guaranteed, tick.rs aims to be free and usable forever. There is no aim to monetize this, it won't sprout ads, or accumulate VC money and become "Counters as a Service". No tracking or other shenanigans. The only things recorded are the ID, the counter, the last modified timestamp and the creation timestamp. IPs are never recorded.

Having said that, there is no express or implied warranty while using this service and I reserve the right to delete or block counters or users for any reason. 
