import requests

baseurl = "http://localhost:3000"

s1 = requests.session()
resp1 = s1.get(f"{baseurl}").text
assert("Waiting" in resp1)

s2 = requests.session()
resp2 = s2.get(f"{baseurl}").text
assert("Joined" in resp2)

s1.post(f"{baseurl}", data="hola")
resp1 = s1.get(f"{baseurl}").text
print(resp1)
